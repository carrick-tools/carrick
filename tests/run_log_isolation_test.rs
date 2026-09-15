//! carrick#1133: a scan's uploaded log holds that scan's lines and no other
//! process's.
//!
//! The field report: during a fifteen-minute scan the uploaded log held 119
//! `Carrick run starting` banners, 118 of them other runs', because the upload
//! read the day's shared debug log from this process's start offset and every
//! carrick process on the machine writes that file.
//!
//! Two real scans run here against one home directory, and they overlap for
//! certain: the first is stopped once it has started logging, the second runs
//! while it is stopped, and the first then finishes writing. Each run's file —
//! the file the upload reads, which `engine`'s own test proves — must hold
//! one banner, its own.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the destination");
    for entry in std::fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

fn scan(repo: &Path, home: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(repo)
        .env("HOME", home)
        .env("CARRICK_MOCK_ALL", "1")
        .env_remove("CARRICK_RUN_ID")
        .env_remove("CARRICK_RUN_PHASE")
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("CI")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a scan")
}

/// Every run file under the home's log directory, with its text.
fn run_files(home: &Path) -> Vec<(PathBuf, String)> {
    let runs = home.join(".carrick").join("logs").join("runs");
    let mut files: Vec<(PathBuf, String)> = std::fs::read_dir(runs)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter_map(|path| std::fs::read_to_string(&path).ok().map(|text| (path, text)))
        .collect();
    files.sort();
    files
}

/// Wait until some run file other than those in `known` carries a banner.
fn wait_for_new_banner(home: &Path, known: &[PathBuf], child: &mut Child) -> PathBuf {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some((path, _)) = run_files(home)
            .into_iter()
            .find(|(path, text)| !known.contains(path) && text.contains("Carrick run starting"))
        {
            return path;
        }
        assert!(
            child.try_wait().expect("poll the scan").is_none(),
            "the scan ended without writing a run file"
        );
        assert!(Instant::now() < deadline, "no run file appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if let Some(status) = child.try_wait().expect("poll the scan") {
            assert!(status.success(), "a scan exited {status:?}");
            return;
        }
        assert!(Instant::now() < deadline, "a scan did not finish");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The run id a banner names.
fn banner_run_id(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| line.contains("Carrick run starting"))
        .expect("a banner");
    let after = line
        .split("run_id=")
        .nth(1)
        .expect("the banner names a run");
    after
        .trim_start_matches('"')
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect()
}

#[test]
fn two_concurrent_scans_each_log_only_their_own_run() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/express-single");
    let home = tempfile::tempdir().expect("temp home");
    let repos = tempfile::tempdir().expect("temp repos");
    let first_repo = repos.path().join("first");
    let second_repo = repos.path().join("second");
    copy_tree(&fixture, &first_repo);
    copy_tree(&fixture, &second_repo);

    let mut first = scan(&first_repo, home.path());
    let first_file = wait_for_new_banner(home.path(), &[], &mut first);
    let first_pid = libc::pid_t::try_from(first.id()).expect("a pid that fits");
    // SAFETY: signals to the child this test started.
    unsafe { libc::kill(first_pid, libc::SIGSTOP) };
    let size_while_stopped = std::fs::metadata(&first_file).expect("run file").len();

    let mut second = scan(&second_repo, home.path());
    let second_file =
        wait_for_new_banner(home.path(), std::slice::from_ref(&first_file), &mut second);
    unsafe { libc::kill(first_pid, libc::SIGCONT) };

    wait(&mut second);
    wait(&mut first);

    let files = run_files(home.path());
    assert_eq!(
        files.len(),
        2,
        "one file per scan: {:?}",
        files.iter().map(|(path, _)| path).collect::<Vec<_>>()
    );
    let first_text = std::fs::read_to_string(&first_file).expect("first run file");
    let second_text = std::fs::read_to_string(&second_file).expect("second run file");
    assert!(
        first_text.len() as u64 > size_while_stopped,
        "the first scan kept writing after the second started, so the two overlapped"
    );

    let first_run = banner_run_id(&first_text);
    let second_run = banner_run_id(&second_text);
    assert_ne!(first_run, second_run);
    for (path, text) in [(&first_file, &first_text), (&second_file, &second_text)] {
        assert_eq!(
            text.matches("Carrick run starting").count(),
            1,
            "{} holds another run's banner:\n{text}",
            path.display()
        );
    }
    assert!(!first_text.contains(&second_run), "{first_text}");
    assert!(!second_text.contains(&first_run), "{second_text}");
    for (path, run) in [(&first_file, &first_run), (&second_file, &second_run)] {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.contains(&run[..8]),
            "{name} does not name its run {run}"
        );
    }

    // And the scans wrote nothing into the day's shared file.
    let logs = home.path().join(".carrick").join("logs");
    let daily_banners: usize = std::fs::read_dir(&logs)
        .expect("log dir")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("carrick.log")
        })
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .map(|text| text.matches("Carrick run starting").count())
        .sum();
    assert_eq!(daily_banners, 0, "a scan wrote into the shared daily log");
}

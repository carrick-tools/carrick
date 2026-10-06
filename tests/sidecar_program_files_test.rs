//! carrick#2027 through the Rust client: a sidecar process given another
//! process's program files, before it is asked anything else, holds the same
//! files in the same order.
//!
//! A process adds a file its tsconfig does not list the first time it is asked
//! about it, so its program's file order follows the order its requests came
//! in, and with `stableTypeOrdering` the compiler orders two same-named types
//! by that file order. The tree has two interfaces named `Dog` in two modules
//! outside the program; the first process is asked about the yard's module
//! first, the second about the kennel's. Needs `node` and the built sidecar
//! (`cd src/sidecar && npm ci && npm run build`), and fails rather than skips
//! in CI without them.

use carrick::services::type_sidecar::{InferKind, InferRequestItem, TypeSidecar};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

const KENNEL: (&str, &str) = (
    "tools/kennel/dog.ts",
    "export interface Dog {\n  bark(): string;\n}\n\nexport function kennel(d: Dog) {\n  return d.bark();\n}\n",
);
const YARD: (&str, &str) = (
    "tools/yard/dog.ts",
    "export interface Dog {\n  woof(): number;\n}\n\nexport function yard(d: Dog) {\n  return d.woof();\n}\n",
);
const PICK: (&str, &str) = (
    "tools/pick.ts",
    "import type { Dog as KennelDog } from './kennel/dog.js';\n\
     import type { Dog as YardDog } from './yard/dog.js';\n\n\
     export function pick(k: KennelDog, y: YardDog, flip: boolean) {\n  return flip ? k : y;\n}\n",
);

/// The built sidecar, or `None` where it cannot run outside CI.
fn sidecar_path() -> Option<PathBuf> {
    let node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    let runnable = node && path.exists();
    assert!(
        runnable || std::env::var("CI").is_err(),
        "node and the built sidecar are needed in CI, or this test would not run"
    );
    runnable.then_some(path)
}

fn write_tree(root: &Path) {
    let files = [
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"ES2022","module":"NodeNext","moduleResolution":"NodeNext","strict":true,"skipLibCheck":true},"include":["src/**/*"]}"#,
        ),
        ("src/index.ts", "export const ready = true;\n"),
        KENNEL,
        YARD,
        PICK,
    ];
    for (file, text) in files {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

fn scoped(sidecar_path: &Path, root: &Path) -> TypeSidecar {
    let sidecar = TypeSidecar::spawn(sidecar_path).expect("spawn sidecar");
    sidecar.start_init(root, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");
    sidecar
}

/// The return type of the function each module declares, asked one module at
/// a time, as the signature pass asks.
fn ask(sidecar: &TypeSidecar, root: &Path, modules: &[(&str, &str)]) -> Vec<String> {
    modules
        .iter()
        .map(|(file, text)| {
            let line = text
                .lines()
                .position(|line| line.starts_with("export function "))
                .expect("a function")
                + 1;
            let item = InferRequestItem {
                file_path: root.join(file).to_string_lossy().into_owned(),
                line_number: line as u32,
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                infer_kind: InferKind::SignatureReturn,
                alias: Some(file.to_string()),
                param_name: None,
            };
            let response = sidecar.infer_types(&[item], None).expect("infer");
            response
                .inferred_types
                .unwrap_or_default()
                .first()
                .map(|inferred| inferred.type_string.clone())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn a_process_given_another_processs_program_files_holds_them_in_its_order() {
    let Some(sidecar_path) = sidecar_path() else {
        eprintln!("Skipping test: node or the built sidecar is missing");
        return;
    };
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    write_tree(&root);

    let first = scoped(&sidecar_path, &root);
    assert_eq!(
        first.program_files().unwrap(),
        Vec::<PathBuf>::new(),
        "no request has built the program yet"
    );
    let first_printed = ask(&first, &root, &[YARD, KENNEL, PICK]);
    let first_files = first.program_files().unwrap();
    let place = |files: &[PathBuf], file: &str| files.iter().position(|f| f == &root.join(file));
    assert!(
        matches!(
            (place(&first_files, YARD.0), place(&first_files, KENNEL.0)),
            (Some(yard), Some(kennel)) if yard < kennel
        ),
        "the yard's module is placed first: {first_files:#?}"
    );

    let second = scoped(&sidecar_path, &root);
    assert_eq!(second.add_program_files(&[]).unwrap(), 0);
    assert_eq!(
        second.add_program_files(&first_files).unwrap(),
        3,
        "the three modules outside the program"
    );
    assert_eq!(second.program_files().unwrap(), first_files);
    let second_printed = ask(&second, &root, &[KENNEL, YARD, PICK]);
    assert_eq!(second.program_files().unwrap(), first_files);
    assert_eq!(second_printed[2], first_printed[2]);
    assert!(
        first_printed[2].contains("KennelDog") && first_printed[2].contains("YardDog"),
        "the union names both interfaces, or the test compares nothing: {first_printed:?}"
    );

    assert_eq!(second.add_program_files(&first_files).unwrap(), 0);
}

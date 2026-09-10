"""A local npm registry proving the Action overrides allowScripts safely."""
import http.server
import io
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import threading

with tempfile.TemporaryDirectory(prefix="carrick-deno-lifecycle-") as temporary:
    root = Path(temporary)
    sentinel = root / "SCRIPT_RAN"
    package = {
        "name": "fixture-dependency",
        "version": "1.0.0",
        "scripts": {"install": "node -e \"require('fs').writeFileSync(" + json.dumps(str(sentinel)).replace('"', "'") + ", 'ran')\""},
    }
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode="w:gz") as tar:
        data = json.dumps(package).encode()
        info = tarfile.TarInfo("package/package.json")
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))

    class Registry(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path.endswith(".tgz"):
                body = archive.getvalue()
            else:
                version = dict(package, dist={"tarball": f"http://127.0.0.1:{self.server.server_port}/fixture.tgz"})
                body = json.dumps({"name": package["name"], "dist-tags": {"latest": "1.0.0"}, "versions": {"1.0.0": version}}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Registry)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        (root / "deno.json").write_text(json.dumps({
            "imports": {"fixture": "npm:fixture-dependency@1.0.0"},
            "nodeModulesDir": "auto", "allowScripts": ["npm:fixture-dependency"], "lock": False,
        }))
        env = dict(os.environ, DENO_DIR=str(root / "cache"), NPM_CONFIG_REGISTRY=f"http://127.0.0.1:{server.server_port}/")
        prepared = subprocess.run(["bash", sys.argv[1], "install", str(root), "deno"], env=env, capture_output=True, text=True, timeout=60)
        assert prepared.returncode == 0 and "Installed" in prepared.stdout, prepared.stdout + prepared.stderr
        assert not sentinel.exists(), "Action ran the dependency lifecycle script"
        assert not (root / "node_modules").exists(), "Action did not force the global cache"
        assert any((root / "cache").rglob("package.json")), "dependency was not prepared"
        # Positive control: exactly this package and config do authorize the
        # install script without the Action's node-modules-dir override.
        control = subprocess.run(["deno", "install", "--frozen"], cwd=root, env=env, capture_output=True, text=True, timeout=60)
        assert control.returncode == 0 and sentinel.exists(), control.stdout + control.stderr
        # A mixed root must still materialize Node dependencies for services
        # that explicitly select an ordinary TypeScript configuration.
        sentinel.unlink()
        shutil.rmtree(root / "node_modules")
        node_root = {"name": "fixture-root", "version": "1.0.0", "dependencies": {"fixture-dependency": "1.0.0"}}
        (root / "package.json").write_text(json.dumps(node_root))
        (root / "package-lock.json").write_text(json.dumps({
            "name": "fixture-root", "version": "1.0.0", "lockfileVersion": 3,
            "packages": {"": node_root, "node_modules/fixture-dependency": {
                "version": "1.0.0", "resolved": f"http://127.0.0.1:{server.server_port}/fixture.tgz", "hasInstallScript": True,
            }},
        }))
        mixed_env = dict(env, DENO_DIR=str(root / "mixed-cache"), npm_config_cache=str(root / "npm-cache"))
        detected = subprocess.run(["bash", sys.argv[1], "detect", str(root)], env=mixed_env, capture_output=True, text=True, timeout=60)
        assert "manager=npm" in detected.stdout, detected.stdout + detected.stderr
        mixed = subprocess.run(["bash", sys.argv[1], "install", str(root), "npm"], env=mixed_env, capture_output=True, text=True, timeout=60)
        assert mixed.returncode == 0 and "with npm" in mixed.stdout and "with deno" in mixed.stdout, mixed.stdout + mixed.stderr
        assert (root / "node_modules/fixture-dependency/package.json").is_file(), "Node install missing"
        assert any((root / "mixed-cache").rglob("package.json")), "Deno cache missing"
        assert not sentinel.exists(), "mixed preparation executed lifecycle script"
        print("Deno lifecycle regression: Action suppressed authorized script; positive control executed it")
    finally:
        server.shutdown()
        server.server_close()

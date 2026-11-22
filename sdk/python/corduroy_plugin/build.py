"""corduroy-py build <module.py> --out <file.wasm> [--capabilities a,b] [--wit <file>]

Builds a plugin module into a WebAssembly component that imports the named capabilities and
nothing else: no WASI.
"""

import argparse
import subprocess
import sys
import tempfile
from pathlib import Path

from corduroy_plugin.world import default_wit, world

ENTRY = "corduroy_entry"


def build(source: Path, out: Path, capabilities: list[str], wit: str) -> None:
    sdk = Path(__file__).resolve().parent.parent
    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        (scratch / "wit").mkdir()
        (scratch / "wit" / "corduroy.wit").write_text(world(wit, capabilities))
        # The runtime first, so the globals are routed before the plugin's module runs.
        (scratch / f"{ENTRY}.py").write_text(
            "import corduroy_plugin._runtime as _runtime\n"
            f"import {source.stem} as _plugin\n"
            "WitWorld = _runtime.world_for(_plugin)\n"
        )
        command = [
            sys.executable,
            "-c",
            "import sys; from componentize_py import script; sys.argv[0] = 'componentize-py'; sys.exit(script())",
            "-d",
            str(scratch / "wit"),
            "-w",
            "generated",
            "componentize",
            "-p",
            str(scratch),
            "-p",
            str(sdk),
            "-p",
            str(source.resolve().parent),
            ENTRY,
            # No WASI: the component imports only the capabilities above.
            "--stub-wasi",
            "-o",
            str(out),
        ]
        subprocess.run(command, check=True)
    listed = ", ".join(capabilities) or "none"
    print(f"built {out} ({out.stat().st_size} bytes; capabilities: {listed})")


def main() -> None:
    parser = argparse.ArgumentParser(prog="corduroy-py")
    commands = parser.add_subparsers(dest="command", required=True)
    command = commands.add_parser("build", help="build a plugin module into a component")
    command.add_argument("source", type=Path)
    command.add_argument("-o", "--out", type=Path, required=True)
    command.add_argument("-c", "--capabilities", default="")
    command.add_argument("--wit", type=Path)
    args = parser.parse_args()
    capabilities = [name.strip() for name in args.capabilities.split(",") if name.strip()]
    wit = args.wit.read_text() if args.wit else default_wit()
    build(args.source, args.out, capabilities, wit)


if __name__ == "__main__":
    main()

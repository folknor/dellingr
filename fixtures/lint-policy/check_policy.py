"""Keep the lint fixture exhaustive and prevent nested Clippy configuration."""

from pathlib import Path
import tomllib


def main():
    root = Path(__file__).resolve().parents[2]
    with (root / "clippy.toml").open("rb") as source:
        policy = tomllib.load(source)
    with Path(__file__).with_name("expect.toml").open("rb") as source:
        manifest = tomllib.load(source)
    expected = {
        ("clippy::" + key.replace("-", "_"), entry["path"])
        for key in ("disallowed-methods", "disallowed-types")
        for entry in policy[key]
    }
    actual = {
        (entry["lint"], entry["path"])
        for file in manifest["files"].values()
        for entry in file.get("expect", [])
    }
    if expected != actual:
        raise SystemExit(f"lint fixture drift: missing={expected - actual}, extra={actual - expected}")
    # Inspect project source, not ignored build caches or upstream checkouts.
    import subprocess

    result = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=root, check=True, capture_output=True,
    )
    nested = sorted({
        path for path in result.stdout.decode().split("\0")
        if Path(path).name in ("clippy.toml", ".clippy.toml") and path != "clippy.toml"
    })
    if nested:
        raise SystemExit(f"nested Clippy configuration shadows policy: {nested}")
    print("ok")


if __name__ == "__main__":
    main()

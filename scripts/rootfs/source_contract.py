"""Keep source checkouts and caller-owned input bundles read-only."""
from pathlib import Path
import sys

SOURCE_ROOT = Path(__file__).resolve().parents[2]

def external_output(value, *inputs):
    path = Path(value).expanduser().resolve()
    for source in (SOURCE_ROOT, *(Path(p).resolve() for p in inputs)):
        if path == source or source in path.parents or path in source.parents:
            raise ValueError(f"output/cache overlaps read-only source: {path} and {source}")
    return path

if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", action="append", default=[])
    parser.add_argument("outputs", nargs="+")
    args = parser.parse_args()
    for value in args.outputs:
        print(external_output(value, *args.input))

#!/usr/bin/env python3
"""Ferritex architecture boundary lint.

Checks:
1. Crate dependency direction: ferritex-core must not depend on ferritex-application or ferritex-infra
2. No cycles in crate dependency graph
3. ferritex-core internal: peer context modules must not import each other's internal modules
   (only kernel and peer api submodules are allowed)
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORE_SRC = ROOT / "crates" / "ferritex-core" / "src"
SHARED_CONTEXTS = {"kernel", "diagnostics", "policy", "compilation"}
TOKEN_RE = re.compile(r"::|{|}|,|\*|as\b|[A-Za-z_][A-Za-z0-9_]*")


def cargo_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        cwd=ROOT,
        capture_output=True,
        check=True,
        text=True,
    )
    return json.loads(result.stdout)


def workspace_graph(metadata: dict) -> dict[str, set[str]]:
    workspace_members = set(metadata["workspace_members"])
    packages = {
        package["id"]: package
        for package in metadata["packages"]
        if package["id"] in workspace_members
    }
    package_names = {
        package_id: package["name"] for package_id, package in packages.items()
    }
    resolve = metadata.get("resolve") or {}
    nodes = {
        node["id"]: node
        for node in resolve.get("nodes", [])
        if node["id"] in workspace_members
    }

    graph = {package["name"]: set() for package in packages.values()}
    for package_id, node in nodes.items():
        package_name = package_names[package_id]
        for dependency in node.get("deps", []):
            dependency_id = dependency["pkg"]
            if dependency_id in package_names:
                graph[package_name].add(package_names[dependency_id])

    return graph


def detect_cycle(graph: dict[str, set[str]]) -> list[str] | None:
    state: dict[str, int] = {}
    stack: list[str] = []

    def visit(node: str) -> list[str] | None:
        state[node] = 1
        stack.append(node)

        for dependency in sorted(graph.get(node, ())):
            dependency_state = state.get(dependency, 0)
            if dependency_state == 1:
                start = stack.index(dependency)
                return stack[start:] + [dependency]
            if dependency_state == 0:
                cycle = visit(dependency)
                if cycle is not None:
                    return cycle

        stack.pop()
        state[node] = 2
        return None

    for node in sorted(graph):
        if state.get(node, 0) == 0:
            cycle = visit(node)
            if cycle is not None:
                return cycle

    return None


def peer_contexts() -> set[str]:
    return {
        path.name
        for path in CORE_SRC.iterdir()
        if path.is_dir() and path.name not in SHARED_CONTEXTS
    }


def iter_use_statements(path: Path) -> list[tuple[int, str]]:
    statements: list[tuple[int, str]] = []
    current: list[str] = []
    start_line: int | None = None

    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        stripped = line.strip()
        if start_line is None:
            if not stripped.startswith("use crate::"):
                continue
            start_line = line_number
            current = [stripped]
        else:
            current.append(stripped)

        if ";" in stripped:
            statements.append((start_line, " ".join(current)))
            current = []
            start_line = None

    return statements


def parse_use_paths(statement: str) -> list[list[str]]:
    prefix = "use crate::"
    if not statement.startswith(prefix):
        return []

    tokens = TOKEN_RE.findall(statement[len(prefix) :].rstrip(";").strip())
    if not tokens:
        return []

    paths, position = parse_use_tree(tokens, 0, [])
    if position != len(tokens):
        raise ValueError(f"unexpected trailing tokens: {tokens[position:]}")
    return paths


def parse_use_tree(
    tokens: list[str], position: int, prefix: list[str]
) -> tuple[list[list[str]], int]:
    if position >= len(tokens):
        raise ValueError("unexpected end of use tree")

    if tokens[position] == "{":
        return parse_use_group(tokens, position + 1, prefix)

    segments: list[str] = []
    while position < len(tokens) and is_identifier(tokens[position]):
        segments.append(tokens[position])
        position += 1

        if position < len(tokens) and tokens[position] == "as":
            position += 1
            if position >= len(tokens) or not is_identifier(tokens[position]):
                raise ValueError("expected alias after `as`")
            position += 1
            return [prefix + segments], position

        if position < len(tokens) and tokens[position] == "::":
            if position + 1 >= len(tokens):
                raise ValueError("unexpected end after `::`")
            next_token = tokens[position + 1]
            if next_token == "{":
                return parse_use_group(tokens, position + 2, prefix + segments)
            if next_token == "*":
                return [prefix + segments + ["*"]], position + 2
            position += 1
            continue

        return [prefix + segments], position

    raise ValueError(f"unexpected token: {tokens[position]!r}")


def parse_use_group(
    tokens: list[str], position: int, prefix: list[str]
) -> tuple[list[list[str]], int]:
    paths: list[list[str]] = []

    while position < len(tokens):
        if tokens[position] == "}":
            return paths, position + 1

        group_paths, position = parse_use_tree(tokens, position, prefix)
        paths.extend(group_paths)

        if position < len(tokens) and tokens[position] == ",":
            position += 1

    raise ValueError("missing closing brace in use tree")


def is_identifier(token: str) -> bool:
    return token not in {"::", "{", "}", ",", "*", "as"}


def check_core_import_boundaries() -> list[str]:
    violations: list[str] = []
    peers = peer_contexts()

    for rust_file in sorted(CORE_SRC.rglob("*.rs")):
        relative = rust_file.relative_to(CORE_SRC)
        if len(relative.parts) < 2:
            continue

        source_context = relative.parts[0]
        if source_context not in peers:
            continue

        for line_number, statement in iter_use_statements(rust_file):
            try:
                paths = parse_use_paths(statement)
            except ValueError as exc:
                violations.append(
                    f"{relative}:{line_number}: could not parse `{statement}` ({exc})"
                )
                continue

            for path in paths:
                target_context = path[0]
                if (
                    target_context in SHARED_CONTEXTS
                    or target_context not in peers
                    or target_context == source_context
                ):
                    continue

                target_submodule = path[1] if len(path) > 1 else None
                if target_submodule != "api":
                    rendered = "::".join(["crate", *path])
                    violations.append(
                        f"{relative}:{line_number}: {source_context} must not import "
                        f"{target_context} internals via `{rendered}`"
                    )

    return violations


def main() -> int:
    try:
        metadata = cargo_metadata()
    except subprocess.CalledProcessError as exc:
        stderr = exc.stderr.strip()
        if stderr:
            print(stderr, file=sys.stderr)
        return exc.returncode or 1

    graph = workspace_graph(metadata)
    errors: list[str] = []

    forbidden_dependencies = {"ferritex-application", "ferritex-infra"}
    core_dependencies = graph.get("ferritex-core", set())
    for dependency in sorted(forbidden_dependencies & core_dependencies):
        errors.append(
            f"ferritex-core must not depend on {dependency} (found dependency edge)"
        )

    cycle = detect_cycle(graph)
    if cycle is not None:
        errors.append(f"crate dependency cycle detected: {' -> '.join(cycle)}")

    errors.extend(check_core_import_boundaries())

    if errors:
        print("Architecture boundary violations detected:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    print("Architecture boundary checks passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

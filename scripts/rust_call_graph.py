#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict, deque
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable


DEFAULT_RUST_ANALYZER = (
    Path.home()
    / ".rustup/toolchains/nightly-x86_64-unknown-linux-gnu/bin/rust-analyzer"
)

DEFAULT_ROOTS = [
    "crate::ingest::ingest_shard",
    "crate::python::py_ingest_shard",
    "crate::router::dispatch",
    "crate::python::PyReader::router_search",
    "crate::python::py_rebuild_tid",
    "crate::python::py_rebuild_bm25",
]

WRAPPER_TYPES = {
    "Arc",
    "Bound",
    "Box",
    "Cow",
    "Mutex",
    "Option",
    "PyResult",
    "Rc",
    "RefCell",
    "Result",
    "RwLock",
}

EXPR_WRAPPERS = {
    "ARRAY_EXPR",
    "ASYNC_EXPR",
    "AWAIT_EXPR",
    "BOX_EXPR",
    "CAST_EXPR",
    "INDEX_EXPR",
    "PAREN_EXPR",
    "PREFIX_EXPR",
    "REF_EXPR",
    "RETURN_EXPR",
    "TRY_EXPR",
    "TUPLE_EXPR",
}


@dataclass
class AstNode:
    kind: str
    node_type: str
    start: int
    end: int
    line: int
    children: list["AstNode"]
    text: str | None = None

    def direct_nodes(self, kind: str | None = None) -> list["AstNode"]:
        out = [child for child in self.children if child.node_type == "Node"]
        if kind is None:
            return out
        return [child for child in out if child.kind == kind]

    def first_direct(self, kind: str) -> "AstNode | None":
        for child in self.children:
            if child.node_type == "Node" and child.kind == kind:
                return child
        return None


@dataclass
class Definition:
    id: str
    module: str
    file: str
    line: int
    name: str
    kind: str
    visibility: str
    impl_type: str | None
    return_type: str | None
    ast: AstNode
    source: bytes
    module_aliases: dict[str, str]
    struct_fields: dict[str, str]


@dataclass
class Edge:
    caller: str
    callee: str
    kind: str
    file: str
    line: int
    expr: str


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a conservative crate-local Rust call graph.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--crate-root",
        default=".",
        help="Path to the workspace root containing Cargo.toml.",
    )
    parser.add_argument(
        "--rust-analyzer-bin",
        default=os.environ.get("RUST_ANALYZER_BIN", str(DEFAULT_RUST_ANALYZER)),
        help="Path to the rust-analyzer binary.",
    )
    parser.add_argument(
        "--out-json",
        default="scratch/rust-call-graph.json",
        help="Machine-readable graph output path.",
    )
    parser.add_argument(
        "--out-dot",
        default="scratch/rust-call-graph.dot",
        help="Graphviz DOT output path.",
    )
    parser.add_argument(
        "--out-md",
        default="scratch/rust-call-graph.md",
        help="Focused Mermaid markdown output path.",
    )
    parser.add_argument(
        "--focus-root",
        action="append",
        default=[],
        help="Root function id for the focused Mermaid graph. Repeat as needed.",
    )
    parser.add_argument(
        "--focus-depth",
        type=int,
        default=3,
        help="Max edge depth from focused roots in the Mermaid graph.",
    )
    return parser.parse_args()


def build_node(raw: dict) -> AstNode:
    return AstNode(
        kind=raw["kind"],
        node_type=raw["type"],
        start=raw["start"][0],
        end=raw["end"][0],
        line=raw["start"][1] + 1,
        children=[build_node(child) for child in raw.get("children", [])],
        text=raw.get("text"),
    )


def iter_nodes(node: AstNode) -> Iterable[AstNode]:
    yield node
    for child in node.children:
        if child.node_type == "Node":
            yield from iter_nodes(child)


def text_slice(source: bytes | str, start: int, end: int) -> str:
    if isinstance(source, bytes):
        return source[start:end].decode("utf-8", errors="replace")
    return source[start:end]


def node_text(node: AstNode, source: bytes | str) -> str:
    return text_slice(source, node.start, node.end)


def first_descendant(node: AstNode, kind: str) -> AstNode | None:
    for desc in iter_nodes(node):
        if desc.kind == kind:
            return desc
    return None


def first_ident_text(node: AstNode, source: bytes | str) -> str | None:
    for desc in iter_nodes(node):
        if desc.kind in {"NAME", "NAME_REF"}:
            text = node_text(desc, source).strip()
            if text:
                return text
    for desc in iter_nodes(node):
        if desc.kind == "IDENT" and desc.text:
            return desc.text
    return None


def load_ast(path: Path, rust_analyzer_bin: str) -> tuple[bytes, AstNode]:
    source = path.read_bytes()
    proc = subprocess.run(
        [rust_analyzer_bin, "parse", "--json"],
        input=source.decode("utf-8"),
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"rust-analyzer parse failed for {path}:\n{proc.stderr.strip() or proc.stdout.strip()}"
        )
    return source, build_node(json.loads(proc.stdout))


def module_path_for(crate_root: Path, path: Path) -> str:
    rel = path.relative_to(crate_root)
    if rel == Path("src/lib.rs"):
        return "crate"
    if rel.parts[:1] == ("src",) and rel.suffix == ".rs":
        return "crate::" + "::".join(rel.with_suffix("").parts[1:])
    raise ValueError(f"Unsupported Rust source location: {path}")


def normalize_path_text(text: str) -> str:
    text = re.sub(r"\s+", "", text)
    out: list[str] = []
    depth = 0
    i = 0
    while i < len(text):
        ch = text[i]
        if ch == '<':
            depth += 1
            i += 1
            continue
        if ch == '>':
            depth = max(depth - 1, 0)
            i += 1
            continue
        if depth == 0:
            out.append(ch)
        i += 1
    normalized = "".join(out)
    while "::::" in normalized:
        normalized = normalized.replace("::::", "::")
    return normalized.strip(":")


def split_top_level(text: str, sep: str = ",") -> list[str]:
    out: list[str] = []
    depth_angle = 0
    depth_brace = 0
    depth_paren = 0
    start = 0
    for idx, ch in enumerate(text):
        if ch == '<':
            depth_angle += 1
        elif ch == '>':
            depth_angle = max(depth_angle - 1, 0)
        elif ch == '{':
            depth_brace += 1
        elif ch == '}':
            depth_brace = max(depth_brace - 1, 0)
        elif ch == '(':
            depth_paren += 1
        elif ch == ')':
            depth_paren = max(depth_paren - 1, 0)
        elif ch == sep and depth_angle == 0 and depth_brace == 0 and depth_paren == 0:
            piece = text[start:idx].strip()
            if piece:
                out.append(piece)
            start = idx + 1
    tail = text[start:].strip()
    if tail:
        out.append(tail)
    return out


def parent_module(module_path: str) -> str:
    if module_path == "crate":
        return "crate"
    return module_path.rsplit("::", 1)[0]


def resolve_relative_module(base_module: str, raw_path: str) -> str:
    parts = [segment for segment in raw_path.split("::") if segment]
    module = base_module
    while parts and parts[0] == "super":
        module = parent_module(module)
        parts = parts[1:]
    if parts and parts[0] == "self":
        parts = parts[1:]
    if parts and parts[0] == "crate":
        return "crate::" + "::".join(parts[1:]) if len(parts) > 1 else "crate"
    if not parts:
        return module
    if module == "crate":
        return "crate::" + "::".join(parts)
    return module + "::" + "::".join(parts)


def expand_use_spec(spec: str, base_prefix: str, out: dict[str, str]) -> None:
    spec = spec.strip()
    if not spec or spec == "*":
        return
    if "{" in spec:
        prefix, rest = spec.split("{", 1)
        inner, _ = rest.rsplit("}", 1)
        prefix = prefix.rstrip(":")
        next_prefix = prefix if not base_prefix else f"{base_prefix}::{prefix}".strip(":")
        for piece in split_top_level(inner):
            expand_use_spec(piece, next_prefix, out)
        return

    alias = None
    if " as " in spec:
        spec, alias = [part.strip() for part in spec.rsplit(" as ", 1)]
    full = spec if not base_prefix else f"{base_prefix}::{spec}".strip(":")
    full = normalize_path_text(full)
    target = full
    local_name = alias or spec.split("::")[-1]
    if local_name == "self":
        local_name = full.split("::")[-1]
    if local_name and not full.endswith("::*"):
        out[local_name] = target


def collect_use_aliases(module_path: str, root: AstNode, source: bytes) -> dict[str, str]:
    aliases: dict[str, str] = {}
    for child in root.direct_nodes():
        if child.kind != "USE":
            continue
        raw = node_text(child, source)
        raw = raw.strip()
        if not raw.startswith("use "):
            continue
        spec = raw[4:].rstrip(";").strip()
        if spec.startswith("crate::"):
            expand_use_spec(spec, "", aliases)
        elif spec.startswith("self::") or spec.startswith("super::"):
            expand_use_spec(resolve_relative_module(module_path, spec), "", aliases)
    return aliases


def type_short_name(
    type_text: str | None,
    aliases: dict[str, str],
    current_impl_type: str | None = None,
) -> str | None:
    if not type_text:
        return None
    text = normalize_path_text(type_text)
    text = text.replace("&mut", "").replace("&", "").replace("mut", "").strip()
    text = re.sub(r"\+'?[A-Za-z_][A-Za-z0-9_]*", "", text)
    if text == "Self":
        return current_impl_type

    for wrapper in WRAPPER_TYPES:
        prefix = f"{wrapper}<"
        if text.startswith(prefix) and text.endswith(">"):
            inner = split_top_level(text[len(prefix) : -1])[0]
            nested = type_short_name(inner, aliases, current_impl_type)
            if nested:
                return nested

    if text.startswith("(") and text.endswith(")"):
        return None
    if text.startswith("dyn "):
        text = text[4:]
    if text.startswith("impl "):
        text = text[5:]
    if text in aliases:
        text = aliases[text]
    text = normalize_path_text(text)

    parts = [part for part in text.split("::") if part]
    if not parts:
        return None
    for part in reversed(parts):
        if part != "Self":
            return part
    return None


def parse_struct_fields(
    node: AstNode,
    source: bytes,
    aliases: dict[str, str],
) -> tuple[str | None, dict[str, str]]:
    name = first_ident_text(node.first_direct("NAME") or node, source)
    fields: dict[str, str] = {}
    field_list = first_descendant(node, "RECORD_FIELD_LIST")
    if not name or field_list is None:
        return name, fields
    for field in field_list.direct_nodes("RECORD_FIELD"):
        field_name = first_ident_text(field.first_direct("NAME") or field, source)
        type_node = next(
            (child for child in field.direct_nodes() if child.kind.endswith("_TYPE")),
            None,
        )
        if field_name and type_node is not None:
            short = type_short_name(node_text(type_node, source), aliases)
            if short:
                fields[field_name] = short
    return name, fields


def parse_impl_header(
    module_path: str,
    node: AstNode,
    source: bytes,
    aliases: dict[str, str],
) -> str | None:
    assoc_list = node.first_direct("ASSOC_ITEM_LIST")
    if assoc_list is None:
        return None
    before_assoc = [
        child for child in node.direct_nodes() if child.end <= assoc_list.start and child.kind.endswith("_TYPE")
    ]
    if not before_assoc:
        return None
    header_text = text_slice(source, node.start, assoc_list.start)
    if " for " in header_text and len(before_assoc) >= 2:
        target = before_assoc[-1]
    else:
        target = before_assoc[0]
    return type_short_name(node_text(target, source), aliases)


def find_fn_name(node: AstNode, source: bytes) -> str | None:
    name_node = node.first_direct("NAME")
    return first_ident_text(name_node or node, source)


def find_param_list(node: AstNode) -> AstNode | None:
    return node.first_direct("PARAM_LIST")


def find_return_type(
    fn_node: AstNode,
    source: bytes,
    aliases: dict[str, str],
    impl_type: str | None,
) -> str | None:
    for child in fn_node.direct_nodes():
        if child.kind == "RET_TYPE":
            return type_short_name(node_text(child, source).lstrip("->"), aliases, impl_type)
    return None


def visibility_for(fn_node: AstNode, source: bytes) -> str:
    fn_kw = fn_node.first_direct("FN_KW") or fn_node
    prefix = text_slice(source, fn_node.start, fn_kw.start)
    return "pub" if "pub" in prefix else "private"


def collect_module_data(
    crate_root: Path,
    rust_analyzer_bin: str,
) -> tuple[
    dict[str, tuple[bytes, AstNode]],
    dict[str, dict[str, str]],
    dict[str, dict[str, dict[str, str]]],
]:
    files: dict[str, tuple[bytes, AstNode]] = {}
    module_aliases: dict[str, dict[str, str]] = {}
    module_struct_fields: dict[str, dict[str, dict[str, str]]] = defaultdict(dict)

    for path in sorted((crate_root / "src").glob("*.rs")):
        source, root = load_ast(path, rust_analyzer_bin)
        module_path = module_path_for(crate_root, path)
        files[module_path] = (source, root)
        aliases = collect_use_aliases(module_path, root, source)
        module_aliases[module_path] = aliases
        for child in root.direct_nodes():
            if child.kind == "STRUCT":
                struct_name, fields = parse_struct_fields(child, source, aliases)
                if struct_name and fields:
                    module_struct_fields[module_path][struct_name] = fields
    return files, module_aliases, module_struct_fields


def collect_definitions(
    crate_root: Path,
    files: dict[str, tuple[bytes, AstNode]],
    module_aliases: dict[str, dict[str, str]],
    module_struct_fields: dict[str, dict[str, dict[str, str]]],
) -> dict[str, Definition]:
    defs: dict[str, Definition] = {}

    for module_path, (source, root) in files.items():
        aliases = module_aliases[module_path]
        struct_fields = module_struct_fields[module_path]
        file_path = str((crate_root / ("src/lib.rs" if module_path == "crate" else module_path.replace("crate::", "src/") + ".rs")).resolve())

        for child in root.direct_nodes():
            if child.kind == "MOD" and find_fn_name(child, source) == "tests":
                continue

            if child.kind == "FN":
                fn_name = find_fn_name(child, source)
                if not fn_name:
                    continue
                def_id = f"{module_path}::{fn_name}"
                defs[def_id] = Definition(
                    id=def_id,
                    module=module_path,
                    file=file_path,
                    line=child.line,
                    name=fn_name,
                    kind="function",
                    visibility=visibility_for(child, source),
                    impl_type=None,
                    return_type=find_return_type(child, source, aliases, None),
                    ast=child,
                    source=source,
                    module_aliases=aliases,
                    struct_fields={},
                )
                continue

            if child.kind != "IMPL":
                continue
            impl_type = parse_impl_header(module_path, child, source, aliases)
            assoc_items = child.first_direct("ASSOC_ITEM_LIST")
            if not impl_type or assoc_items is None:
                continue
            for item in assoc_items.direct_nodes("FN"):
                fn_name = find_fn_name(item, source)
                if not fn_name:
                    continue
                def_id = f"{module_path}::{impl_type}::{fn_name}"
                defs[def_id] = Definition(
                    id=def_id,
                    module=module_path,
                    file=file_path,
                    line=item.line,
                    name=fn_name,
                    kind="method",
                    visibility=visibility_for(item, source),
                    impl_type=impl_type,
                    return_type=find_return_type(item, source, aliases, impl_type),
                    ast=item,
                    source=source,
                    module_aliases=aliases,
                    struct_fields=struct_fields.get(impl_type, {}),
                )
    return defs


def build_indexes(defs: dict[str, Definition]) -> dict[str, dict]:
    free_by_module_name: dict[tuple[str, str], list[str]] = defaultdict(list)
    free_by_name: dict[str, list[str]] = defaultdict(list)
    assoc_by_type_name: dict[tuple[str, str], list[str]] = defaultdict(list)
    assoc_by_module_type_name: dict[tuple[str, str, str], list[str]] = defaultdict(list)

    for defn in defs.values():
        if defn.impl_type:
            assoc_by_type_name[(defn.impl_type, defn.name)].append(defn.id)
            assoc_by_module_type_name[(defn.module, defn.impl_type, defn.name)].append(defn.id)
        else:
            free_by_module_name[(defn.module, defn.name)].append(defn.id)
            free_by_name[defn.name].append(defn.id)

    return {
        "free_by_module_name": free_by_module_name,
        "free_by_name": free_by_name,
        "assoc_by_type_name": assoc_by_type_name,
        "assoc_by_module_type_name": assoc_by_module_type_name,
    }


def extract_param_env(defn: Definition) -> dict[str, str]:
    env: dict[str, str] = {}
    params = find_param_list(defn.ast)
    if params is None:
        return env
    for param in params.direct_nodes():
        if param.kind == "SELF_PARAM" and defn.impl_type:
            env["self"] = defn.impl_type
            continue
        if param.kind != "PARAM":
            continue
        name_node = first_descendant(param, "IDENT_PAT")
        type_node = next(
            (child for child in param.direct_nodes() if child.kind.endswith("_TYPE")),
            None,
        )
        if name_node is None or type_node is None:
            continue
        name = first_ident_text(name_node, defn.source)
        short = type_short_name(
            node_text(type_node, defn.source),
            defn.module_aliases,
            defn.impl_type,
        )
        if name and short:
            env[name] = short
    return env


def unwrap_expression(node: AstNode) -> AstNode:
    current = node
    while current.kind in EXPR_WRAPPERS:
        next_node = next((child for child in current.direct_nodes()), None)
        if next_node is None:
            break
        current = next_node
    return current


def infer_expr_type(
    expr: AstNode | None,
    defn: Definition,
    env: dict[str, str],
    defs: dict[str, Definition],
    indexes: dict[str, dict],
) -> str | None:
    if expr is None:
        return None
    expr = unwrap_expression(expr)

    if expr.kind == "PATH_EXPR":
        text = normalize_path_text(node_text(expr, defn.source))
        if text == "self":
            return defn.impl_type
        if text == "Self":
            return defn.impl_type
        if text in env:
            return env[text]
        if text in defn.module_aliases:
            return type_short_name(defn.module_aliases[text], defn.module_aliases, defn.impl_type)
        if "::" not in text:
            return type_short_name(text, defn.module_aliases, defn.impl_type)
        prefix = text.rsplit("::", 1)[0]
        return type_short_name(prefix, defn.module_aliases, defn.impl_type)

    if expr.kind == "FIELD_EXPR":
        receiver = next((child for child in expr.direct_nodes()), None)
        field_name = first_ident_text(expr, defn.source)
        receiver_type = infer_expr_type(receiver, defn, env, defs, indexes)
        if receiver_type and field_name:
            return defn.struct_fields.get(field_name) or None
        return None

    if expr.kind == "CALL_EXPR":
        callee_node = next((child for child in expr.direct_nodes() if child.kind.endswith("EXPR")), None)
        callee_text = normalize_path_text(node_text(callee_node, defn.source)) if callee_node else ""
        resolved = resolve_call_target(callee_text, defn, env, defs, indexes)
        if resolved and defs[resolved].return_type:
            return defs[resolved].return_type
        if "::" in callee_text:
            return type_short_name(callee_text.rsplit("::", 1)[0], defn.module_aliases, defn.impl_type)
        return None

    return None


def resolve_call_target(
    callee_text: str,
    defn: Definition,
    env: dict[str, str],
    defs: dict[str, Definition],
    indexes: dict[str, dict],
) -> str | None:
    if not callee_text:
        return None

    if callee_text in defn.module_aliases:
        callee_text = defn.module_aliases[callee_text]

    if callee_text.startswith("crate::"):
        return callee_text if callee_text in defs else None

    if callee_text.startswith("self::") or callee_text.startswith("super::"):
        candidate = resolve_relative_module(defn.module, callee_text)
        return candidate if candidate in defs else None

    if callee_text.startswith("Self::") and defn.impl_type:
        suffix = callee_text.split("::", 1)[1]
        candidates = indexes["assoc_by_module_type_name"].get(
            (defn.module, defn.impl_type, suffix),
            []
        )
        if len(candidates) == 1:
            return candidates[0]
        candidates = indexes["assoc_by_type_name"].get((defn.impl_type, suffix), [])
        if len(candidates) == 1:
            return candidates[0]
        return None

    segments = [segment for segment in callee_text.split("::") if segment]
    if len(segments) == 1:
        candidates = indexes["free_by_module_name"].get((defn.module, segments[0]), [])
        if len(candidates) == 1:
            return candidates[0]
        candidates = indexes["free_by_name"].get(segments[0], [])
        if len(candidates) == 1:
            return candidates[0]
        return None

    first = segments[0]
    last = segments[-1]

    if first in defn.module_aliases:
        aliased = defn.module_aliases[first]
        candidate = normalize_path_text(aliased + "::" + "::".join(segments[1:]))
        if candidate in defs:
            return candidate
        aliased_type = type_short_name(aliased, defn.module_aliases, defn.impl_type)
        if aliased_type:
            candidates = indexes["assoc_by_type_name"].get((aliased_type, last), [])
            if len(candidates) == 1:
                return candidates[0]

    type_candidates = indexes["assoc_by_type_name"].get((first, last), [])
    if len(type_candidates) == 1:
        return type_candidates[0]

    current_module_candidate = resolve_relative_module(defn.module, callee_text)
    if current_module_candidate in defs:
        return current_module_candidate

    crate_candidate = "crate::" + callee_text
    if crate_candidate in defs:
        return crate_candidate

    return None


def resolve_method_target(
    receiver_type: str | None,
    method_name: str,
    defn: Definition,
    indexes: dict[str, dict],
) -> str | None:
    if receiver_type:
        candidates = indexes["assoc_by_module_type_name"].get(
            (defn.module, receiver_type, method_name),
            []
        )
        if len(candidates) == 1:
            return candidates[0]
        candidates = indexes["assoc_by_type_name"].get((receiver_type, method_name), [])
        if len(candidates) == 1:
            return candidates[0]
    return None


def call_expr_text(expr: AstNode, source: str) -> str:
    return normalize_path_text(node_text(expr, source))


def collect_edges(
    defs: dict[str, Definition],
    indexes: dict[str, dict],
) -> tuple[list[Edge], list[dict[str, object]]]:
    edges: list[Edge] = []
    unresolved: list[dict[str, object]] = []

    def visit(node: AstNode, defn: Definition, env: dict[str, str]) -> None:
        if node.kind == "BLOCK_EXPR" and node is not defn.ast.first_direct("BLOCK_EXPR"):
            env = dict(env)

        if node.kind == "LET_STMT":
            initializer = next(
                (child for child in reversed(node.direct_nodes()) if child.kind.endswith("EXPR")),
                None,
            )
            visit_children = [child for child in node.direct_nodes() if child is initializer]
            for child in visit_children:
                visit(child, defn, env)

            name_node = first_descendant(node, "IDENT_PAT")
            explicit_type = next(
                (child for child in node.direct_nodes() if child.kind.endswith("_TYPE")),
                None,
            )
            name = first_ident_text(name_node, defn.source) if name_node else None
            if name:
                inferred = type_short_name(
                    node_text(explicit_type, defn.source) if explicit_type else "",
                    defn.module_aliases,
                    defn.impl_type,
                ) if explicit_type else infer_expr_type(initializer, defn, env, defs, indexes)
                if inferred:
                    env[name] = inferred
            return

        if node.kind == "CALL_EXPR":
            callee_node = next(
                (child for child in node.direct_nodes() if child.kind.endswith("EXPR")),
                None,
            )
            callee_text = call_expr_text(callee_node, defn.source) if callee_node else ""
            target = resolve_call_target(callee_text, defn, env, defs, indexes)
            if target:
                edges.append(
                    Edge(
                        caller=defn.id,
                        callee=target,
                        kind="call",
                        file=defn.file,
                        line=node.line,
                        expr=callee_text,
                    )
                )
            elif callee_text:
                unresolved.append(
                    {
                        "caller": defn.id,
                        "kind": "call",
                        "file": defn.file,
                        "line": node.line,
                        "expr": callee_text,
                    }
                )

        if node.kind == "METHOD_CALL_EXPR":
            children = node.direct_nodes()
            receiver = children[0] if children else None
            name_ref = next((child for child in children if child.kind == "NAME_REF"), None)
            method_name = first_ident_text(name_ref, defn.source) if name_ref else None
            receiver_type = infer_expr_type(receiver, defn, env, defs, indexes)
            if method_name:
                target = resolve_method_target(receiver_type, method_name, defn, indexes)
                expr = f"{node_text(receiver, defn.source).strip()}.{method_name}" if receiver else method_name
                if target:
                    edges.append(
                        Edge(
                            caller=defn.id,
                            callee=target,
                            kind="method_call",
                            file=defn.file,
                            line=node.line,
                            expr=normalize_path_text(expr),
                        )
                    )
                else:
                    unresolved.append(
                        {
                            "caller": defn.id,
                            "kind": "method_call",
                            "file": defn.file,
                            "line": node.line,
                            "expr": normalize_path_text(expr),
                            "receiver_type": receiver_type,
                        }
                    )

        for child in node.direct_nodes():
            visit(child, defn, env)

    for defn in defs.values():
        body = defn.ast.first_direct("BLOCK_EXPR")
        if body is None:
            continue
        env = extract_param_env(defn)
        visit(body, defn, env)

    unique = {
        (edge.caller, edge.callee, edge.kind, edge.line, edge.expr): edge for edge in edges
    }
    return sorted(unique.values(), key=lambda e: (e.caller, e.line, e.callee)), unresolved


def short_label(def_id: str) -> str:
    parts = def_id.split("::")
    if len(parts) <= 3:
        return def_id
    return "::".join(parts[-3:])


def module_edges(edges: list[Edge], defs: dict[str, Definition]) -> list[dict[str, object]]:
    counts: Counter[tuple[str, str]] = Counter()
    for edge in edges:
        src = defs[edge.caller].module
        dst = defs[edge.callee].module
        if src != dst:
            counts[(src, dst)] += 1
    return [
        {"from": src, "to": dst, "count": count}
        for (src, dst), count in sorted(counts.items())
    ]


def reachable_subgraph(
    roots: list[str],
    edges: list[Edge],
    max_depth: int,
) -> tuple[set[str], list[Edge]]:
    by_caller: dict[str, list[Edge]] = defaultdict(list)
    for edge in edges:
        by_caller[edge.caller].append(edge)

    seen: set[str] = set()
    kept_edges: list[Edge] = []
    queue: deque[tuple[str, int]] = deque((root, 0) for root in roots)
    while queue:
        node, depth = queue.popleft()
        if node in seen or depth > max_depth:
            continue
        seen.add(node)
        if depth == max_depth:
            continue
        for edge in by_caller.get(node, []):
            kept_edges.append(edge)
            queue.append((edge.callee, depth + 1))
    for edge in kept_edges:
        seen.add(edge.callee)
        seen.add(edge.caller)
    return seen, kept_edges


def mermaid_id(name: str) -> str:
    return "n" + re.sub(r"[^A-Za-z0-9_]", "_", name)


def render_module_mermaid(module_edge_rows: list[dict[str, object]]) -> str:
    lines = ["flowchart LR"]
    modules = set()
    for row in module_edge_rows:
        modules.add(row["from"])
        modules.add(row["to"])
    for module in sorted(modules):
        lines.append(f'  {mermaid_id(str(module))}["{module}"]')
    for row in module_edge_rows:
        lines.append(
            f'  {mermaid_id(str(row["from"]))} -->|{row["count"]}| {mermaid_id(str(row["to"]))}'
        )
    return "\n".join(lines)


def render_focus_mermaid(
    roots: list[str],
    focus_nodes: set[str],
    focus_edges: list[Edge],
) -> str:
    lines = ["flowchart LR"]
    for node in sorted(focus_nodes):
        label = short_label(node)
        extra = ":::root" if node in roots else ""
        lines.append(f'  {mermaid_id(node)}["{label}"]{extra}')
    for edge in focus_edges:
        lines.append(f"  {mermaid_id(edge.caller)} --> {mermaid_id(edge.callee)}")
    if roots:
        lines.append("  classDef root fill:#f0efe0,stroke:#8a6f00,stroke-width:2px;")
    return "\n".join(lines)


def write_json(
    out_path: Path,
    defs: dict[str, Definition],
    edges: list[Edge],
    unresolved: list[dict[str, object]],
    module_edge_rows: list[dict[str, object]],
) -> None:
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "analysis": {
            "mode": "static",
            "engine": "rust-analyzer parse --json + conservative local resolution",
            "scope": "crate-local library sources under src/*.rs",
        },
        "definitions": [
            {
                "id": defn.id,
                "module": defn.module,
                "file": defn.file,
                "line": defn.line,
                "name": defn.name,
                "kind": defn.kind,
                "visibility": defn.visibility,
                "impl_type": defn.impl_type,
                "return_type": defn.return_type,
            }
            for defn in sorted(defs.values(), key=lambda d: d.id)
        ],
        "edges": [
            {
                "caller": edge.caller,
                "callee": edge.callee,
                "kind": edge.kind,
                "file": edge.file,
                "line": edge.line,
                "expr": edge.expr,
            }
            for edge in edges
        ],
        "unresolved_calls": unresolved,
        "module_edges": module_edge_rows,
    }
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(payload, indent=2, sort_keys=False) + "\n")


def write_dot(out_path: Path, defs: dict[str, Definition], edges: list[Edge]) -> None:
    lines = ["digraph rust_call_graph {", "  rankdir=LR;"]
    for defn in sorted(defs.values(), key=lambda d: d.id):
        lines.append(f'  "{defn.id}" [label="{short_label(defn.id)}"];')
    for edge in edges:
        lines.append(f'  "{edge.caller}" -> "{edge.callee}";')
    lines.append("}")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(lines) + "\n")


def write_markdown(
    out_path: Path,
    defs: dict[str, Definition],
    edges: list[Edge],
    unresolved: list[dict[str, object]],
    module_edge_rows: list[dict[str, object]],
    roots: list[str],
    focus_nodes: set[str],
    focus_edges: list[Edge],
) -> None:
    root_summary = ", ".join(short_label(root) for root in roots) if roots else "none"
    lines = [
        "# Rust Call Graph",
        "",
        "Conservative static analysis over `src/*.rs`, using `rust-analyzer parse --json` for syntax and crate-local name resolution heuristics.",
        "",
        f"- Definitions: {len(defs)}",
        f"- Resolved edges: {len(edges)}",
        f"- Unresolved call sites: {len(unresolved)}",
        f"- Focus roots: {root_summary}",
        "",
        "## Module Overview",
        "",
        "```mermaid",
        render_module_mermaid(module_edge_rows),
        "```",
        "",
        "## Focused Entry Flows",
        "",
        "```mermaid",
        render_focus_mermaid(roots, focus_nodes, focus_edges),
        "```",
        "",
        "## Notes",
        "",
        "- Only crate-local resolved calls are rendered as edges.",
        "- Dynamic dispatch, macro expansion, trait-object calls, and external crate calls are intentionally excluded.",
        "- `unresolved_calls` in the JSON output shows the call sites that need stronger semantic analysis if you want a fuller graph.",
        "",
    ]
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(lines))


def main() -> int:
    args = parse_args()
    crate_root = Path(args.crate_root).resolve()
    rust_analyzer_bin = str(Path(args.rust_analyzer_bin).expanduser())

    if not Path(rust_analyzer_bin).exists():
        print(f"rust-analyzer not found at {rust_analyzer_bin}", file=sys.stderr)
        return 1
    if not (crate_root / "Cargo.toml").exists():
        print(f"No Cargo.toml at {crate_root}", file=sys.stderr)
        return 1

    files, module_aliases, module_struct_fields = collect_module_data(
        crate_root,
        rust_analyzer_bin,
    )
    defs = collect_definitions(crate_root, files, module_aliases, module_struct_fields)
    indexes = build_indexes(defs)
    edges, unresolved = collect_edges(defs, indexes)
    module_edge_rows = module_edges(edges, defs)

    roots = args.focus_root or [root for root in DEFAULT_ROOTS if root in defs]
    focus_nodes, focus_edges = reachable_subgraph(roots, edges, args.focus_depth)

    write_json(Path(args.out_json), defs, edges, unresolved, module_edge_rows)
    write_dot(Path(args.out_dot), defs, edges)
    write_markdown(
        Path(args.out_md),
        defs,
        edges,
        unresolved,
        module_edge_rows,
        roots,
        focus_nodes,
        focus_edges,
    )

    print(
        json.dumps(
            {
                "definitions": len(defs),
                "edges": len(edges),
                "unresolved_calls": len(unresolved),
                "roots": roots,
                "out_json": str(Path(args.out_json)),
                "out_dot": str(Path(args.out_dot)),
                "out_md": str(Path(args.out_md)),
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

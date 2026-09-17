"""The reader behind the op-boundary contract, on sources written to trip it.

`scripts/test-runtime-op-boundary-contract.sh` compares three sets this reader
derives. A reader that silently drops an op -- because a string held a bracket,
an entry was path-qualified, or a macro stamped the function out -- makes that
comparison agree about less than the engine has, which is a gate passing on a
smaller question. Each case below is a shape the real engine source contains.
"""

from __future__ import annotations

import importlib.util
import pathlib
import sys
import tempfile
import textwrap
import unittest

_MODULE_PATH = pathlib.Path(__file__).resolve().parents[1].parent / "lib" / "runtime_ops.py"
_spec = importlib.util.spec_from_file_location("runtime_ops", _MODULE_PATH)
runtime_ops = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = runtime_ops
_spec.loader.exec_module(runtime_ops)


class RuntimeOpsReaderTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self._temp.name)
        self.src = self.root / runtime_ops.RUNTIME_SRC
        self.src.mkdir(parents=True)

    def tearDown(self) -> None:
        self._temp.cleanup()

    def write(self, relative: str, text: str) -> None:
        path = self.src / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(text), encoding="utf-8")

    def test_brackets_and_comment_markers_inside_literals_are_not_structure(self) -> None:
        self.write(
            "a/mod.rs",
            '''
            const URL: &str = "http://example/{not a brace";
            const RAW: &str = r#"a ] ) } "quoted" [ "#;
            const OPEN: char = '{';
            // extension!(commented_out, ops = [op_ghost]);
            /* nested /* block */ extension!(also_commented, ops = [op_ghost2]); */
            deno_core::extension!(
                host_a,
                ops = [op_real, module::op_qualified, op_generic<Foo>, op_turbofish::<Bar>],
                esm = [dir "src/a", "01_a.js"],
            );
            ''',
        )
        ops, _ = runtime_ops.surface(self.root)
        self.assertEqual(sorted(ops), ["op_generic", "op_qualified", "op_real", "op_turbofish"])
        self.assertTrue(all(op.extension == "host_a" for op in ops.values()))

    def test_test_modules_and_test_files_do_not_register_the_surface(self) -> None:
        self.write(
            "b/mod.rs",
            """
            extension!(host_b, ops = [op_kept]);
            #[cfg(test)]
            mod tests {
                deno_core::extension!(fixture, ops = [op_fixture]);
                fn x() { let s = "}"; }
            }
            """,
        )
        self.write("tests/helper.rs", "extension!(helper, ops = [op_helper]);")
        self.write("b/thing_tests.rs", "extension!(thing, ops = [op_thing]);")
        ops, _ = runtime_ops.surface(self.root)
        self.assertEqual(sorted(ops), ["op_kept"])

    def test_definitions_give_shape_including_macros_and_futures(self) -> None:
        self.write(
            "c/mod.rs",
            """
            extension!(host_c, ops = [op_fast, op_async, op_lazy, op_writes, op_stamped, op_plain]);
            #[op2(fast)]
            pub fn op_fast(state: &mut OpState, #[smi] id: u32) -> u32 { id }
            #[op2(async)]
            pub async fn op_async() -> Result<f64, E> { Ok(1.0) }
            #[op2(fast)]
            pub fn op_lazy() -> Result<impl std::future::Future<Output = ()> + use<>, E> { todo!() }
            #[deno_core::op2(fast)]
            pub fn op_writes(#[buffer] out: &mut [u8]) {}
            macro_rules! stamp { ($name:ident) => { #[op2(fast)] pub fn $name() {} }; }
            stamp!(op_stamped);
            #[op2]
            #[serde]
            pub fn op_plain(state: &mut OpState) -> Vec<(String, u32)> where T: Clone { vec![] }
            """,
        )
        ops, _ = runtime_ops.surface(self.root)
        self.assertEqual(ops["op_fast"].attributes, ["fast"])
        self.assertEqual(ops["op_fast"].returns, "u32")
        self.assertFalse(ops["op_fast"].is_async)
        self.assertTrue(ops["op_async"].is_async)
        self.assertTrue(ops["op_lazy"].is_async, "a fast op returning a future is awaited")
        self.assertTrue(ops["op_writes"].writes_buffer)
        self.assertEqual(ops["op_writes"].returns, "")
        self.assertEqual(ops["op_stamped"].attributes, ["macro:stamp"])
        self.assertEqual(ops["op_plain"].returns, "Vec<(String, u32)>")
        self.assertTrue(all(op.source for op in ops.values()))

    def test_imports_follow_aliases_and_the_ops_object(self) -> None:
        self.write("d/mod.rs", "extension!(host_d, ops = [op_one, op_two, op_three, op_unused]);")
        self.write(
            "d/01_d.js",
            """
            import {
                op_one,
                op_two as renamed,
            } from "ext:core/ops";
            // import { op_unused } from "ext:core/ops";
            const { ops } = core;
            ops.op_three();
            """,
        )
        ops, imports = runtime_ops.surface(self.root)
        self.assertEqual(sorted(imports), ["op_one", "op_three", "op_two"])
        self.assertEqual(ops["op_unused"].imported_by, [])
        self.assertEqual(ops["op_two"].imported_by, ["engine/crates/runtime-v8/src/d/01_d.js"])

    def test_one_op_registered_by_two_extensions_is_refused(self) -> None:
        self.write("e/mod.rs", "extension!(host_e, ops = [op_twice]);")
        self.write("f/mod.rs", "extension!(host_f, ops = [op_twice]);")
        with self.assertRaises(ValueError):
            runtime_ops.surface(self.root)


if __name__ == "__main__":
    unittest.main()

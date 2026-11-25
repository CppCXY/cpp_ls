import importlib.util
import pathlib
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("crate_paths", pathlib.Path(__file__).with_name("crate_paths.py"))
crate_paths = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(crate_paths)

SOURCE = """//! [`crate::DocThing`] stays a rustdoc link.
use crate::{Known};
fn root() {
    let _ = crate::Known::Yes(());
    let _ = crate::module::Thing;
}
#[cfg(test)]
mod tests {
    fn test() {
        let _ = crate::module::Thing;
    }
}
macro_rules! definition {
    () => {
        let _ = crate::module::Thing;
    };
}
"""

class CratePathsTest(unittest.TestCase):
    def test_fix_skips_macros_and_puts_imports_in_test_scope(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory, "lib.rs")
            path.write_text(SOURCE, encoding="utf-8")
            self.assertEqual(0, crate_paths.fix([path], verify=False))
            text = path.read_text(encoding="utf-8")
            self.assertIn("[`crate::DocThing`]", text)
            self.assertIn("use crate::module;", text)
            self.assertIn("mod tests {\n    use crate::module;", text)
            self.assertIn("macro_rules! definition {\n    () => {\n        let _ = crate::module::Thing;", text)
            self.assertEqual(2, text.count("use crate::module;"))
            self.assertEqual(0, text.count("use crate::Known;"))

if __name__ == "__main__":
    unittest.main()

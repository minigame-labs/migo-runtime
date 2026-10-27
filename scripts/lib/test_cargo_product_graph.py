import unittest

from cargo_product_graph import compare, parse


class ProductGraphTests(unittest.TestCase):
    def test_distinct_versions_cannot_masquerade_as_the_same_package(self):
        difference = compare("codec v1.0.0|std", "codec v2.0.0|std")
        self.assertEqual(len(difference), 2)
        self.assertTrue(all(name == "codec" for name, _ in difference))

    def test_a_second_version_cannot_hide_the_first_versions_missing_feature(self):
        before = "runtime v1.0.0|net\nruntime v2.0.0|std\n"
        after = "runtime v1.0.0|\nruntime v2.0.0|std\n"
        self.assertEqual(compare(before, after), [
            ("runtime", "v1.0.0 features differ: apple-only=['net'] reference-only=[]")
        ])

    def test_repeated_paths_and_display_markers_do_not_change_the_graph(self):
        self.assertEqual(compare(
            "mine v0.9.19 (/one path)|io,net\nmine v0.9.19 (/one path)|io,net (*)",
            "mine v0.9.19 (/other path)|net,io",
        ), [])
        self.assertEqual(len(parse("mine v1.0|\nmine v2.0|\nmine v1.0| (*)")), 2)

    def test_empty_or_malformed_graphs_fail_closed(self):
        for text in ("", "not a Cargo package", "mine v1.0", "mine v1.0|std|other"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                parse(text)


if __name__ == "__main__":
    unittest.main()

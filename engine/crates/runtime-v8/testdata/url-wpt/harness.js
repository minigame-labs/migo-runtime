// Minimal synchronous testharness adapter. The upstream test files are unchanged.
globalThis.__wpt = { passed: 0, excluded: [], failures: [] };
globalThis.test = (callback, name) => {
    // These APIs are outside Migo's selected surface. Do not emulate DOM or
    // multipart FormData just to make the URL fixture appear more complete.
    if (name === "URLSearchParams constructor, DOMException as argument" ||
        name === "URLSearchParams constructor, FormData.") {
        __wpt.excluded.push(name); return;
    }
    try { callback(); __wpt.passed++; }
    catch (error) { __wpt.failures.push([name, String(error)]); }
};
globalThis.assert_equals = (actual, expected, detail = "") => {
    if (!Object.is(actual, expected)) throw new Error(detail + ": " + JSON.stringify(actual) + " !== " + JSON.stringify(expected));
};
globalThis.assert_array_equals = (actual, expected, detail = "") => {
    assert_equals(actual.length, expected.length, detail + " length");
    for (let i = 0; i < actual.length; i++) assert_equals(actual[i], expected[i], detail + " item " + i);
};
globalThis.assert_true = (value, detail) => assert_equals(value, true, detail);
globalThis.assert_false = (value, detail) => assert_equals(value, false, detail);
globalThis.assert_unreached = detail => { throw new Error(detail); };
globalThis.assert_throws_js = (constructor, callback, detail) => {
    try { callback(); } catch (error) {
        if (error instanceof constructor) return;
        throw new Error(detail + ": wrong exception " + error);
    }
    throw new Error(detail + ": expected exception");
};

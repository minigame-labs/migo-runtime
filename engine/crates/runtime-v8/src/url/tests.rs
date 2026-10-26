use deno_core::{JsRuntime, RuntimeOptions, extension};

extension!(
    url_test_bridge,
    deps = [host_v8_url],
    esm_entry_point = "ext:url_test_bridge/setup.js",
    esm = ["ext:url_test_bridge/setup.js" = {
        source = r#"
        import { URL, URLSearchParams } from 'ext:host_v8_url/03_url.js';
        globalThis.URL = URL; globalThis.URLSearchParams = URLSearchParams;
        globalThis.check = (ok, detail) => { if (!ok) throw new Error(detail); };
    "#
    }],
);

fn runtime() -> JsRuntime {
    crate::icu::install_icu_data();
    JsRuntime::new(RuntimeOptions {
        extensions: vec![super::host_v8_url::init(), url_test_bridge::init()],
        ..Default::default()
    })
}

#[test]
fn wpt_url_constructors_and_setters() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|rt| rt.enter());
    let mut rt = runtime();
    let constructors = include_str!("../../testdata/url-wpt/urltestdata.json");
    let setters = include_str!("../../testdata/url-wpt/setters_tests.json");
    let script = format!(
        r#"
        const failures = [];
        let constructors = 0, setters = 0;
        for (const row of {constructors}) {{
            if (typeof row === 'string') continue;
            constructors++;
            try {{
                let url;
                try {{ url = new URL(row.input, row.base === null ? undefined : row.base); }}
                catch (error) {{ if (row.failure && error instanceof TypeError) continue; throw error; }}
                check(!row.failure, 'expected invalid URL');
                for (const key of ['href','origin','protocol','username','password','host','hostname','port','pathname','search','hash']) {{
                    if (key in row) check(url[key] === row[key], key + ': ' + JSON.stringify(url[key]) + ' !== ' + JSON.stringify(row[key]));
                }}
            }} catch (error) {{ failures.push(['constructor', row.input, error.message]); }}
        }}
        for (const [field, rows] of Object.entries({setters})) {{
            if (field === 'comment') continue;
            for (const row of rows) {{
                setters++;
                try {{
                    const url = new URL(row.href); url[field] = row.new_value;
                    for (const [key, value] of Object.entries(row.expected)) check(url[key] === value, key + ': ' + JSON.stringify(url[key]) + ' !== ' + JSON.stringify(value));
                }} catch (error) {{ failures.push([field, row.href, row.new_value, error.message]); }}
            }}
        }}
        check(failures.length === 0, JSON.stringify({{constructors, setters, failures}}));
    "#
    );
    rt.execute_script("wpt-url", script).unwrap();
}

#[test]
fn live_query_lists_form_codec_reentry_and_coercion() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|rt| rt.enter());
    let mut rt = runtime();
    rt.execute_script("url-query-policy", r#"
        const url = new URL('../b/./c?x=1&x=2&q=a+b', 'https://user:pass@EXAMPLE.com:443/a/d/');
        check(url.href === 'https://user:pass@example.com/a/b/c?x=1&x=2&q=a+b', 'resolution');
        check(url.origin === 'https://example.com', 'origin excludes credentials');
        const params = url.searchParams;
        check(params === url.searchParams && params.getAll('x').join(',') === '1,2', 'stable live list');
        check(params.get('q') === 'a b', 'plus decoding');
        params.append('x', '3'); params.set('q', '~ +');
        check(url.search === '?x=1&x=2&q=%7E+%2B&x=3', 'form encoding and duplicates');
        params.delete('x', '2'); check(params.getAll('x').join(',') === '1,3', 'value-filtered delete');
        params.set('x', '4'); check(params.getAll('x').join(',') === '4', 'set removes later duplicates');
        url.href = 'https://other.test/?z=2&a=3&a=1';
        check(params === url.searchParams && params.get('z') === '2', 'href updates retained params');
        params.sort(); check(url.search === '?a=3&a=1&z=2', 'stable code-unit sort');
        const iterator = params.entries(); iterator.next(); url.search = '?new=1&second=2';
        check(iterator.next().value[0] === 'second', 'iterator observes replacement list');
        url.port = {toString() { url.hostname = 'reentered.test'; return '8443'; }};
        check(url.host === 'reentered.test:8443', 'setter reentry');
        const p = new URLSearchParams('bad=%FF%00&percent=%&empty&=v');
        check(p.get('bad') === '\uFFFD\x00' && p.get('percent') === '%' && p.get('empty') === '', 'malformed form decoding');
        p.set('\uD800', '\uDC00'); check(p.has('\uFFFD', '\uFFFD'), 'USVString values');
        check(URL.canParse('https://[::1]/') && URL.parse('bad') === null, 'safe parse helpers');
        check(new URLSearchParams(null).size === 0, 'null selects empty record');
        let iteratorReads = 0;
        new URLSearchParams({get [Symbol.iterator]() { iteratorReads++; return function* () { yield ['a', 'b']; }; }});
        check(iteratorReads === 1, 'iterator method read once');
        params.toString = () => 'forged=1';
        params.append('owned', 'value');
        check(url.search.includes('owned=value') && !url.search.includes('forged'), 'updates bypass public serializer overrides');
        for (const action of [() => new URL(), () => new URL(Symbol()), () => params.append('a'), () => new URLSearchParams([['a']])]) {
            let caught = false; try { action(); } catch (e) { caught = e instanceof TypeError; }
            check(caught, 'required argument/coercion failure');
        }
    "#).unwrap();
}

#[test]
fn wpt_url_search_params() {
    let executor = crate::tests::support::v8_test_executor();
    let _enter = executor.as_ref().map(|rt| rt.enter());
    let mut rt = runtime();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/url-wpt");
    rt.execute_script(
        "wpt-testharness-adapter",
        std::fs::read_to_string(root.join("harness.js")).unwrap(),
    )
    .unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("urlsearchparams-")
        })
        .collect();
    paths.sort();
    assert_eq!(
        paths.len(),
        10,
        "review coverage when updating upstream fixtures"
    );
    for path in paths {
        let source = std::fs::read_to_string(&path).unwrap();
        rt.execute_script(
            "wpt-url-search-params",
            format!("(() => {{\n{source}\n}})()"),
        )
        .unwrap();
    }
    rt.execute_script(
        "wpt-url-search-params-result",
        r#"
        check(__wpt.excluded.length === 2, 'review selected surface exclusions');
        check(__wpt.passed === 84 && __wpt.failures.length === 0, JSON.stringify(__wpt));
    "#,
    )
    .unwrap();
}

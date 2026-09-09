import XCTest

@testable import MigoAppleCore

#if canImport(JavaScriptCore)
    import JavaScriptCore

    /// The shim, executed rather than pattern-matched.
    ///
    /// The generator emits JavaScript, and a string assertion on generated
    /// JavaScript checks that the generator wrote what the test expected -- not that
    /// the result runs. A `SyntaxError` in the shim takes the whole content-facing
    /// surface with it and shows up as a game that never starts, so the shim is
    /// evaluated here in JavaScriptCore, which both platforms ship and which needs
    /// no device and no WebKit.
    final class MigoWebKitBridgeShimTests: XCTestCase {

        private typealias Surface = MigoWebKitSurface
        private typealias Shim = MigoWebKitBridgeShim

        /// A context with a recording stand-in for `webkit.messageHandlers`.
        ///
        /// `named` are the handlers that exist. A served method whose handler is
        /// absent from this list is exercising the host-bug path on purpose.
        private func context(withHandlers named: [String]) throws -> JSContext {
            let context = try XCTUnwrap(JSContext())
            var thrown: [String] = []
            context.exceptionHandler = { _, value in
                thrown.append(value?.toString() ?? "unknown")
            }
            let handlers = named.map {
                "    \($0.debugDescription): { postMessage: (p) => { "
                    + "globalThis.__posted.push([\($0.debugDescription), p]); "
                    + "return Promise.resolve('ok'); } },"
            }.joined(separator: "\n")
            context.evaluateScript(
                """
                globalThis.__posted = [];
                globalThis.webkit = { messageHandlers: {
                \(handlers)
                } };
                """)
            XCTAssertEqual(thrown, [], "the test's own fixture raised")
            return context
        }

        private func evaluate(
            _ script: String, in context: JSContext, file: StaticString = #filePath,
            line: UInt = #line
        ) -> JSValue? {
            var thrown: [String] = []
            context.exceptionHandler = { _, value in thrown.append(value?.toString() ?? "unknown") }
            let value = context.evaluateScript(script)
            XCTAssertEqual(thrown, [], "the script raised", file: file, line: line)
            return value
        }

        // MARK: - the shipping surface

        func testTheDefaultShimRunsAndInstallsItsNamespace() throws {
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            XCTAssertEqual(
                evaluate("typeof globalThis.\(Shim.namespace)", in: context)?.toString(), "object")
        }

        func testTheEngineNamespaceIsNotPublishedInThisLane() throws {
            // Content that feature-detects `typeof migo === "object"` and then calls
            // migo.createCanvas() would find an object and no canvas. This lane has
            // no engine, so it must not answer to the engine's name.
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            XCTAssertEqual(evaluate("typeof globalThis.migo", in: context)?.toString(), "undefined")
        }

        func testAServedMethodPostsToItsOwnHandler() throws {
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            _ = evaluate("\(Shim.namespace).diagnostics.report({ level: 'error' })", in: context)
            let posted = evaluate("JSON.stringify(globalThis.__posted)", in: context)?.toString()
            XCTAssertEqual(posted, #"[["migo_diagnostics_report",{"level":"error"}]]"#)
        }

        func testACallWithNoArgumentPostsNullRatherThanNothing() throws {
            // `postMessage(undefined)` is not a serialisable message, and the
            // failure would surface as a rejected promise inside WebKit rather than
            // as anything the host can explain.
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            _ = evaluate("\(Shim.namespace).environment.get()", in: context)
            XCTAssertEqual(
                evaluate("JSON.stringify(globalThis.__posted)", in: context)?.toString(),
                #"[["migo_environment_get",null]]"#)
        }

        func testAWithheldMethodThrowsAndSaysWhoWithheldIt() throws {
            let surface = try Surface.configured(withholding: [.diagnostics])
            let context = try context(withHandlers: Shim.handlerNames(for: surface))
            _ = evaluate(Shim.script(for: surface), in: context)
            let message = evaluate(
                """
                (() => { try { \(Shim.namespace).diagnostics.report({}); return 'did not throw'; }
                         catch (error) { return String(error.message); } })()
                """, in: context)?.toString()
            let text = try XCTUnwrap(message)
            XCTAssertTrue(text.contains("withheld"), text)
            XCTAssertTrue(
                text.contains("webkit-full-surface.json"), "the message must name the file "
                    + "that decides, so a host app debugging its content knows where to look: \(text)")
            XCTAssertTrue(
                text.contains("can enable it"),
                "a withheld capability is a configuration answer, not a missing feature: \(text)")
        }

        func testAnAbsentCapabilityIsNowhereInTheShim() throws {
            let script = Shim.script(for: .default)
            for capability in Surface.Capability.allCases
            where Surface.entry(for: capability).provenance == .absent {
                XCTAssertFalse(
                    script.contains(capability.rawValue),
                    "\(capability.rawValue) has no mechanism, and a stub of any kind implies there "
                        + "is one to turn on")
            }
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(script, in: context)
            XCTAssertEqual(
                evaluate("typeof \(Shim.namespace).bluetooth", in: context)?.toString(), "undefined")
        }

        func testContentCannotReplaceTheShim() throws {
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)

            // Whether the attempt *throws* is the caller's business, not the
            // shim's: a non-writable assignment raises a TypeError in strict mode
            // and fails silently in sloppy mode, and content chooses which mode it
            // runs in. The first version of this test asserted the throw and failed
            // against a shim that was in fact holding -- the assignment was
            // discarded and the wrapper simply never raised. So the assertion is on
            // the effect, and the strict-mode diagnostic is checked separately
            // because it is what a developer sees when they try.
            for attempt in [
                "globalThis.\(Shim.namespace) = {}",
                "\(Shim.namespace).diagnostics = {}",
                "\(Shim.namespace).diagnostics.report = () => 'mine'",
                "delete \(Shim.namespace).diagnostics",
            ] {
                _ = evaluate(
                    "(() => { try { \(attempt); } catch (e) {} })()", in: context)
                XCTAssertEqual(
                    evaluate("typeof \(Shim.namespace).diagnostics.report", in: context)?.toString(),
                    "function", "the shim did not survive: \(attempt)")
                _ = evaluate("globalThis.__posted = []", in: context)
                _ = evaluate("\(Shim.namespace).diagnostics.report(1)", in: context)
                XCTAssertEqual(
                    evaluate("JSON.stringify(globalThis.__posted)", in: context)?.toString(),
                    #"[["migo_diagnostics_report",1]]"#,
                    "the method still exists but no longer reaches its handler after: \(attempt)")
            }

            // And under strict mode, which is what a module or a class body gets by
            // default, the same attempt is an error the developer can see.
            let strict = evaluate(
                """
                (() => { 'use strict';
                   try { \(Shim.namespace).diagnostics.report = () => 'mine'; return 'accepted'; }
                   catch (error) { return error.constructor.name; } })()
                """, in: context)?.toString()
            XCTAssertEqual(strict, "TypeError")
        }

        func testAHandlerTheHostForgotToInstallRejectsRatherThanHanging() throws {
            // A served method with no handler is a host bug. Content awaiting a
            // promise nobody settles hangs on a splash screen with nothing in any
            // log; a rejection names the handler.
            let context = try context(withHandlers: [])
            _ = evaluate(Shim.script(for: .default), in: context)
            _ = evaluate(
                """
                globalThis.__outcome = 'pending';
                \(Shim.namespace).diagnostics.report(null).then(
                  () => { globalThis.__outcome = 'resolved'; },
                  (error) => { globalThis.__outcome = String(error.message); });
                """, in: context)
            let outcome = try XCTUnwrap(evaluate("globalThis.__outcome", in: context)?.toString())
            XCTAssertTrue(
                outcome.contains("migo_diagnostics_report"),
                "the rejection has to name the missing handler: \(outcome)")
        }

        func testTheHandlerNamesAreTheOnesTheShimPosts() {
            let script = Shim.script(for: .default)
            let names = Shim.handlerNames(for: .default)
            XCTAssertEqual(names.count, Surface.default.servedBridgeMethods.count)
            for name in names {
                XCTAssertTrue(script.contains("'\(name)'"), "\(name) is installed and never posted to")
            }
        }

        // MARK: - subscriptions

        func testASubscriptionRegistersAndTheHostDeliversToIt() throws {
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            _ = evaluate(
                """
                globalThis.__events = [];
                \(Shim.namespace).lifecycle.observe((event) => { globalThis.__events.push(event); });
                """, in: context)
            // Registering tells the host somebody is listening; without that the
            // host has to evaluate JavaScript into a page that may have no listener.
            XCTAssertEqual(
                evaluate("JSON.stringify(globalThis.__posted)", in: context)?.toString(),
                #"[["migo_lifecycle_observe",{"channel":"lifecycle"}]]"#)

            let delivered = evaluate(
                Shim.deliveryScript(channel: "lifecycle", payloadJSON: #"{"phase":"background"}"#),
                in: context)
            XCTAssertEqual(delivered?.toInt32(), 1)
            XCTAssertEqual(
                evaluate("JSON.stringify(globalThis.__events)", in: context)?.toString(),
                #"[{"phase":"background"}]"#)
        }

        func testTheChannelTheHostAddressesIsTheOneTheShimRegistered() {
            // Two derivations of one name is a listener that is registered and never
            // called, and nothing anywhere reports it.
            XCTAssertEqual(Shim.channel(forMethod: "lifecycle.observe"), "lifecycle")
            XCTAssertEqual(Shim.channel(forMethod: "a.b.observe"), "a.b")
            XCTAssertEqual(Shim.channel(forMethod: "bare"), "bare")
        }

        func testDeliveringToNobodyIsHarmless() throws {
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            let delivered = evaluate(
                Shim.deliveryScript(channel: "lifecycle", payloadJSON: "{}"), in: context)
            XCTAssertEqual(
                delivered?.toInt32(), 0,
                "a host that delivers before content subscribed must get a count, not an error")
        }

        func testOneThrowingListenerDoesNotSwallowTheEventForTheRest() throws {
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            _ = evaluate(
                """
                globalThis.__seen = 0;
                \(Shim.namespace).lifecycle.observe(() => { throw new Error('first'); });
                \(Shim.namespace).lifecycle.observe(() => { globalThis.__seen += 1; });
                """, in: context)
            let delivered = evaluate(
                Shim.deliveryScript(channel: "lifecycle", payloadJSON: #"{"phase":"suspend"}"#),
                in: context)
            XCTAssertEqual(
                evaluate("globalThis.__seen", in: context)?.toInt32(), 1,
                "a suspend that reaches half its listeners is a save that half happened")
            XCTAssertEqual(delivered?.toInt32(), 1, "the count reports who actually received it")
        }

        func testSubscribingWithSomethingUncallableIsRefused() throws {
            let context = try context(withHandlers: Shim.handlerNames(for: .default))
            _ = evaluate(Shim.script(for: .default), in: context)
            let outcome = evaluate(
                """
                (() => { try { \(Shim.namespace).lifecycle.observe({}); return 'accepted'; }
                         catch (error) { return error.constructor.name; } })()
                """, in: context)?.toString()
            XCTAssertEqual(
                outcome, "TypeError",
                "a registration the host can never call has to fail where it was made, not the "
                    + "first time the app goes to the background")
        }

        func testASubscriptionIsNotShapedLikeACall() {
            // The shapes are declared, so this checks the generator honours the
            // declaration rather than picking one.
            let script = Shim.script(for: .default)
            XCTAssertTrue(script.contains("subscribe('lifecycle', 'migo_lifecycle_observe'"))
            XCTAssertTrue(script.contains("post('migo_environment_get'"))
            XCTAssertFalse(
                script.contains("subscribe('environment'"),
                "a call built as a subscription registers a listener nobody calls")
        }

        // MARK: - tables the shipping one cannot currently produce

        func testTwoMethodsUnderOneGroupStillParse() throws {
            // The shipping table has one method per group, so a generator that
            // emitted `const g_content` twice would be a SyntaxError nothing here
            // could reach. Handed a table that shares a group, it must still run.
            let entries = [
                Surface.Entry(
                    .diagnostics, .hostBridge, defaultEnabled: true,
                    bridgeMethod: "content.read", bridgeKind: .call),
                Surface.Entry(
                    .environment, .hostBridge, defaultEnabled: true,
                    bridgeMethod: "content.describe", bridgeKind: .call),
            ]
            let script = Shim.script(for: .default, entries: entries)
            let context = try context(withHandlers: [
                "migo_content_read", "migo_content_describe",
            ])
            _ = evaluate(script, in: context)
            XCTAssertEqual(
                evaluate("typeof \(Shim.namespace).content.read", in: context)?.toString(),
                "function")
            XCTAssertEqual(
                evaluate("typeof \(Shim.namespace).content.describe", in: context)?.toString(),
                "function")
        }

        func testTwoGroupsSharingALeafNameDoNotCollide() throws {
            let entries = [
                Surface.Entry(
                    .diagnostics, .hostBridge, defaultEnabled: true,
                    bridgeMethod: "a.shared.read", bridgeKind: .call),
                Surface.Entry(
                    .environment, .hostBridge, defaultEnabled: true,
                    bridgeMethod: "b.shared.read", bridgeKind: .call),
            ]
            let context = try context(withHandlers: [
                "migo_a_shared_read", "migo_b_shared_read",
            ])
            _ = evaluate(Shim.script(for: .default, entries: entries), in: context)
            _ = evaluate("\(Shim.namespace).a.shared.read(1); \(Shim.namespace).b.shared.read(2)", in: context)
            XCTAssertEqual(
                evaluate("JSON.stringify(globalThis.__posted)", in: context)?.toString(),
                #"[["migo_a_shared_read",1],["migo_b_shared_read",2]]"#,
                "one identifier for two different parent objects puts the second method on the "
                    + "first one's parent")
        }

        func testAMethodNameCarryingAQuoteCannotEndTheLiteral() throws {
            // Nothing in the shipping table contains a quote, and that is the
            // reason to check it: the escaping is only ever exercised by a name
            // nobody has added yet.
            let entries = [
                Surface.Entry(
                    .diagnostics, .hostBridge, defaultEnabled: false,
                    bridgeMethod: #"weird'); globalThis.__escaped = true; ('"#,
                    bridgeKind: .call)
            ]
            let context = try context(withHandlers: [])
            _ = evaluate(Shim.script(for: .default, entries: entries), in: context)
            XCTAssertEqual(
                evaluate("typeof globalThis.__escaped", in: context)?.toString(), "undefined",
                "a method name ended the string literal and ran as code in the page world")
        }
    }
#endif

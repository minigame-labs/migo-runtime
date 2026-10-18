import { op_console } from "ext:core/ops";
import { primordials } from "ext:core/mod.js";

const { DateNow, SafeMap, MapPrototypeGet, MapPrototypeSet, MapPrototypeHas, MapPrototypeDelete } = primordials;

let groupDepth = 0;

// An Error's own properties are not enumerable, so JSON writes one as `{}`: a
// game's `console.error(e)` reached the log with neither the message nor where
// it was thrown. It is written as its name and message and then its stack --
// V8's stack already starts with those, JavaScriptCore's (Performance+) does
// not. JSON also has no text for undefined, a function or a symbol, and those
// read as their String.
function formatArg(a) {
    if (typeof a === "string") return a;
    try {
        if (a instanceof Error) {
            const head = String(a);
            const stack = a.stack;
            if (typeof stack !== "string" || stack === "") return head;
            return stack.startsWith(head) ? stack : head + "\n" + stack;
        }
        return JSON.stringify(a) ?? String(a);
    } catch (_) {
        // The op's answer for a value with no text, so a log call never throws.
        try { return String(a); } catch (_) { return "<invalid value>"; }
    }
}

function formatArgs(args) {
    return "  ".repeat(groupDepth) + args.map(formatArg).join(" ");
}

// The console namespace of the Console Standard, all of it: content calls the
// members a browser has whether or not the mini-game reference lists them, and a
// member that is missing is a TypeError that ends the game over a log line --
// Cocos Creator 3 calls console.time while it starts its physics. What a log
// cannot do (a table's layout, a clear screen) it does as near as a log can.
const counts = new SafeMap();
const timers = new SafeMap();

function labelOf(label) {
    return label === undefined ? "default" : String(label);
}

class Console {
    debug(...args) {
        op_console(formatArgs(args), 0);
    }
    log(...args) {
        op_console(formatArgs(args), 1);
    }
    info(...args) {
        op_console(formatArgs(args), 1);
    }
    warn(...args) {
        op_console(formatArgs(args), 2);
    }
    error(...args) {
        op_console(formatArgs(args), 3);
    }
    assert(condition, ...args) {
        if (condition) return;
        if (args.length === 0) {
            op_console(formatArgs(["Assertion failed"]), 3);
        } else if (typeof args[0] === "string") {
            args[0] = "Assertion failed: " + args[0];
            op_console(formatArgs(args), 3);
        } else {
            op_console(formatArgs(["Assertion failed:", ...args]), 3);
        }
    }
    clear() {
        // A log has no screen to clear, and its earlier lines are the record.
    }
    count(label) {
        const name = labelOf(label);
        const next = (MapPrototypeGet(counts, name) ?? 0) + 1;
        MapPrototypeSet(counts, name, next);
        op_console(formatArgs([name + ": " + next]), 1);
    }
    countReset(label) {
        const name = labelOf(label);
        if (MapPrototypeHas(counts, name)) {
            MapPrototypeSet(counts, name, 0);
        } else {
            op_console(formatArgs(["Count for '" + name + "' does not exist"]), 2);
        }
    }
    dir(item) {
        op_console(formatArgs([item]), 1);
    }
    dirxml(...args) {
        op_console(formatArgs(args), 1);
    }
    table(data, properties) {
        op_console(formatArgs(properties === undefined ? [data] : [data, properties]), 1);
    }
    trace(...args) {
        const stack = new Error().stack;
        const frames = typeof stack === "string" ? stack.slice(stack.indexOf("\n") + 1) : "";
        const head = args.length === 0 ? ["Trace"] : ["Trace: " + formatArg(args[0]), ...args.slice(1)];
        op_console(formatArgs(head) + (frames ? "\n" + frames : ""), 1);
    }
    group(...label) {
        op_console(formatArgs(label), 1);
        groupDepth++;
    }
    groupCollapsed(...label) {
        this.group(...label);
    }
    groupEnd() {
        if (groupDepth > 0) groupDepth--;
    }
    time(label) {
        const name = labelOf(label);
        if (MapPrototypeHas(timers, name)) {
            op_console(formatArgs(["Timer '" + name + "' already exists"]), 2);
            return;
        }
        MapPrototypeSet(timers, name, DateNow());
    }
    timeLog(label, ...args) {
        const name = labelOf(label);
        if (!MapPrototypeHas(timers, name)) {
            op_console(formatArgs(["Timer '" + name + "' does not exist"]), 2);
            return;
        }
        op_console(formatArgs([name + ": " + (DateNow() - MapPrototypeGet(timers, name)) + "ms", ...args]), 1);
    }
    timeEnd(label) {
        const name = labelOf(label);
        if (!MapPrototypeHas(timers, name)) {
            op_console(formatArgs(["Timer '" + name + "' does not exist"]), 2);
            return;
        }
        const elapsed = DateNow() - MapPrototypeGet(timers, name);
        MapPrototypeDelete(timers, name);
        op_console(formatArgs([name + ": " + elapsed + "ms"]), 1);
    }
}

const console = new Console();
export { console };

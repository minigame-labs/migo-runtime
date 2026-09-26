import { op_console } from "ext:core/ops";

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
    group(label) {
        op_console("  ".repeat(groupDepth) + (label ?? ""), 1);
        groupDepth++;
    }
    groupEnd() {
        if (groupDepth > 0) groupDepth--;
    }
}

const console = new Console();
export { console };

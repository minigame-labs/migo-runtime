import { op_console } from "ext:core/ops";
import { primordials } from "ext:core/mod.js";

const {
    DateNow, NumberParseFloat, NumberParseInt, SafeMap, MapPrototypeGet, MapPrototypeSet, MapPrototypeHas,
    MapPrototypeDelete,
} = primordials;

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

// A conversion the formatter makes may run content's own toString; a log line never throws for it.
function convert(fn, value) {
    try {
        return fn(value);
    } catch (_) {
        return "<invalid value>";
    }
}

function toText(value) {
    return typeof value === "symbol" ? convert((v) => v.toString(), value) : convert(String, value);
}

function toInteger(value) {
    return typeof value === "symbol" ? "NaN" : String(convert((v) => NumberParseInt(v, 10), value));
}

function toFloat(value) {
    return typeof value === "symbol" ? "NaN" : String(convert((v) => NumberParseFloat(v), value));
}

// The Console Standard's Formatter: a string first argument's %s, %d, %i, %f, %o, %O and %c each
// take the next argument (%c's is CSS, which a log has no use for), %% is a percent sign, and what
// is left over follows, space-separated. Scanned by hand, character by character.
function format(args) {
    if (args.length < 2 || typeof args[0] !== "string") {
        return args.map(formatArg).join(" ");
    }
    const template = args[0];
    let out = "";
    let next = 1;
    for (let i = 0; i < template.length; i++) {
        const ch = template[i];
        if (ch !== "%" || i + 1 >= template.length) {
            out += ch;
            continue;
        }
        const spec = template[i + 1];
        if (spec === "%") {
            out += "%";
            i++;
            continue;
        }
        const known = spec === "s" || spec === "d" || spec === "i" || spec === "f" || spec === "o" || spec === "O"
            || spec === "c";
        if (next >= args.length || !known) {
            out += ch;
            continue;
        }
        const value = args[next++];
        i++;
        if (spec === "s") out += toText(value);
        else if (spec === "d" || spec === "i") out += toInteger(value);
        else if (spec === "f") out += toFloat(value);
        else if (spec === "o" || spec === "O") out += formatArg(value);
        // %c: styling, consumed and dropped.
    }
    const rest = args.slice(next).map(formatArg);
    return rest.length === 0 ? out : out + " " + rest.join(" ");
}

function formatArgs(args) {
    return "  ".repeat(groupDepth) + format(args);
}

// The standard's Printer, for the members that print without formatting (timeLog's data).
function printArgs(args) {
    return "  ".repeat(groupDepth) + args.map(formatArg).join(" ");
}

function group(label) {
    op_console(formatArgs(label), 1);
    groupDepth++;
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
        // The group stack empties, as the standard asks; a log has no screen to clear, and its
        // earlier lines are the record.
        groupDepth = 0;
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
        // `Error` and its prepareStackTrace are content's to replace; a trace without its frames is
        // still a log line, and a log line never throws.
        let frames = "";
        try {
            const stack = new Error().stack;
            if (typeof stack === "string") frames = stack.slice(stack.indexOf("\n") + 1);
        } catch (_) {
            frames = "";
        }
        const head = args.length === 0 ? ["Trace"] : ["Trace: " + formatArg(args[0]), ...args.slice(1)];
        op_console(formatArgs(head) + (frames ? "\n" + frames : ""), 1);
    }
    // Neither depends on its receiver: a logger that holds `console.groupCollapsed` apart from the
    // console calls it with none.
    group(...label) {
        group(label);
    }
    groupCollapsed(...label) {
        group(label);
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
        op_console(printArgs([name + ": " + (DateNow() - MapPrototypeGet(timers, name)) + "ms", ...args]), 1);
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

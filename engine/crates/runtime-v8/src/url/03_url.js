import { primordials } from "ext:core/mod.js";
import { op_url_parse, op_url_parts, op_url_set, op_url_search_parse, op_url_search_serialize, toWellFormed } from "ext:host_v8_url/00_url_backend.js";

const {
    ArrayFrom, ArrayPrototypePush, ArrayPrototypeSort, ArrayPrototypeSplice,
    ObjectDefineProperty, ObjectGetOwnPropertyDescriptor, ReflectOwnKeys,
    ReflectApply, SafeMap, String, StringPrototypeSlice, StringPrototypeStartsWith,
    SymbolIterator, SymbolToStringTag, TypeError,
} = primordials;

function usv(value) {
    if (typeof value === "symbol") throw new TypeError("Cannot convert a Symbol to a string");
    return toWellFormed(String(value));
}
function required(count, minimum) {
    if (count < minimum) throw new TypeError("Not enough arguments");
}
function parseQuery(query) {
    return op_url_search_parse(StringPrototypeStartsWith(query, "?") ? StringPrototypeSlice(query, 1) : query);
}

let replaceQuery, linkQuery;
class URLSearchParams {
    #list = [];
    #update = null;
    constructor(init = "") {
        // Friend functions use private fields without exposing mutation hooks.
        // Keep this in the constructor: class static blocks require newer
        // WebKit than the product's iOS 15 floor.
        if (replaceQuery === undefined) {
            replaceQuery = (params, query) => { params.#list = parseQuery(query); };
            linkQuery = (params, update) => { params.#update = update; };
        }
        if (init === null) return;
        if (typeof init === "object" || typeof init === "function") {
            const method = init[SymbolIterator];
            if (method !== undefined && method !== null) {
                if (typeof method !== "function") throw new TypeError("Query initializer is not iterable");
                const iterable = { [SymbolIterator]() { return ReflectApply(method, init, []); } };
                for (const pair of iterable) {
                    if ((typeof pair !== "object" && typeof pair !== "function") || pair === null) {
                        throw new TypeError("Query pair must be an iterable object");
                    }
                    const pairMethod = pair[SymbolIterator];
                    if (typeof pairMethod !== "function") throw new TypeError("Query pair must be iterable");
                    const values = ArrayFrom({ [SymbolIterator]() { return ReflectApply(pairMethod, pair, []); } }, usv);
                    if (values.length !== 2) throw new TypeError("Query pair must have two items");
                    ArrayPrototypePush(this.#list, values);
                }
            } else {
                const positions = new SafeMap();
                const keys = ReflectOwnKeys(init);
                for (let i = 0; i < keys.length; i++) {
                    const key = keys[i];
                    const descriptor = ObjectGetOwnPropertyDescriptor(init, key);
                    if (!descriptor || !descriptor.enumerable) continue;
                    const name = usv(key), value = usv(init[key]);
                    // WebIDL record conversion collapses equal USVString keys
                    // before creating the query list; order stays at first use.
                    if (positions.has(name)) this.#list[positions.get(name)][1] = value;
                    else { positions.set(name, this.#list.length); ArrayPrototypePush(this.#list, [name, value]); }
                }
            }
        } else this.#list = parseQuery(usv(init));
    }
    #changed() { if (this.#update) this.#update(op_url_search_serialize(this.#list)); }
    get size() { return this.#list.length; }
    append(name, value) {
        required(arguments.length, 2);
        ArrayPrototypePush(this.#list, [usv(name), usv(value)]);
        this.#changed();
    }
    delete(name, value = undefined) {
        required(arguments.length, 1); name = usv(name);
        if (value !== undefined) value = usv(value);
        for (let i = 0; i < this.#list.length;) {
            const pair = this.#list[i];
            if (pair[0] === name && (value === undefined || pair[1] === value)) ArrayPrototypeSplice(this.#list, i, 1);
            else i++;
        }
        this.#changed();
    }
    get(name) {
        required(arguments.length, 1); name = usv(name);
        for (let i = 0; i < this.#list.length; i++) if (this.#list[i][0] === name) return this.#list[i][1];
        return null;
    }
    getAll(name) {
        required(arguments.length, 1); name = usv(name);
        const values = [];
        for (let i = 0; i < this.#list.length; i++) if (this.#list[i][0] === name) ArrayPrototypePush(values, this.#list[i][1]);
        return values;
    }
    has(name, value = undefined) {
        required(arguments.length, 1); name = usv(name);
        if (value !== undefined) value = usv(value);
        for (let i = 0; i < this.#list.length; i++) {
            const pair = this.#list[i];
            if (pair[0] === name && (value === undefined || pair[1] === value)) return true;
        }
        return false;
    }
    set(name, value) {
        required(arguments.length, 2); name = usv(name); value = usv(value);
        let found = false;
        for (let i = 0; i < this.#list.length;) {
            const pair = this.#list[i];
            if (pair[0] !== name) { i++; continue; }
            if (!found) { pair[1] = value; found = true; i++; }
            else ArrayPrototypeSplice(this.#list, i, 1);
        }
        if (!found) ArrayPrototypePush(this.#list, [name, value]);
        this.#changed();
    }
    sort() {
        ArrayPrototypeSort(this.#list, (a, b) => a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0);
        this.#changed();
    }
    *entries() { for (let i = 0; i < this.#list.length; i++) yield [this.#list[i][0], this.#list[i][1]]; }
    *keys() { for (let i = 0; i < this.#list.length; i++) yield this.#list[i][0]; }
    *values() { for (let i = 0; i < this.#list.length; i++) yield this.#list[i][1]; }
    forEach(callback, thisArg = undefined) {
        if (typeof callback !== "function") throw new TypeError("Callback must be a function");
        for (let i = 0; i < this.#list.length; i++) ReflectApply(callback, thisArg, [this.#list[i][1], this.#list[i][0], this]);
    }
    toString() { return op_url_search_serialize(this.#list); }
    [SymbolIterator]() { return this.entries(); }
    get [SymbolToStringTag]() { return "URLSearchParams"; }
}

class URL {
    #native;
    #parts = null;
    #params = null;
    constructor(input, base = undefined) {
        required(arguments.length, 1); input = usv(input);
        if (base !== undefined) base = usv(base);
        this.#native = op_url_parse(input, base);
    }
    #read(index) {
        if (this.#parts === null) this.#parts = op_url_parts(this.#native);
        return this.#parts[index];
    }
    #write(index, value) {
        // Content toString can reenter; no native URL borrow spans conversion.
        this.#parts = op_url_set(this.#native, index, usv(value));
        if (this.#params !== null && (index === 0 || index === 9)) replaceQuery(this.#params, this.#parts[9]);
    }
    get href() { return this.#read(0); }
    set href(value) { this.#write(0, value); }
    get origin() { return this.#read(1); }
    get protocol() { return this.#read(2); }
    set protocol(value) { this.#write(2, value); }
    get username() { return this.#read(3); }
    set username(value) { this.#write(3, value); }
    get password() { return this.#read(4); }
    set password(value) { this.#write(4, value); }
    get host() { return this.#read(5); }
    set host(value) { this.#write(5, value); }
    get hostname() { return this.#read(6); }
    set hostname(value) { this.#write(6, value); }
    get port() { return this.#read(7); }
    set port(value) { this.#write(7, value); }
    get pathname() { return this.#read(8); }
    set pathname(value) { this.#write(8, value); }
    get search() { return this.#read(9); }
    set search(value) { this.#write(9, value); }
    get hash() { return this.#read(10); }
    set hash(value) { this.#write(10, value); }
    get searchParams() {
        if (this.#params === null) {
            this.#params = new URLSearchParams(this.#read(9));
            linkQuery(this.#params, query => {
                this.#parts = op_url_set(this.#native, 9, query === "" ? "" : "?" + query);
            });
        }
        return this.#params;
    }
    toString() { return this.#read(0); }
    toJSON() { return this.#read(0); }
    get [SymbolToStringTag]() { return "URL"; }
    static canParse(input, base = undefined) {
        required(arguments.length, 1); input = usv(input);
        if (base !== undefined) base = usv(base);
        try { op_url_parse(input, base); return true; } catch (_) { return false; }
    }
    static parse(input, base = undefined) {
        required(arguments.length, 1); input = usv(input);
        if (base !== undefined) base = usv(base);
        try { return new URL(input, base); } catch (_) { return null; }
    }
    static createObjectURL() { throw new TypeError("createObjectURL is not supported in this environment"); }
    static revokeObjectURL() { throw new TypeError("revokeObjectURL is not supported in this environment"); }
}
export { URL, URLSearchParams };

// WebIDL iterable uses the same function for entries and @@iterator.
ObjectDefineProperty(URLSearchParams.prototype, SymbolIterator, {
    value: URLSearchParams.prototype.entries, writable: true, enumerable: true, configurable: true,
});
for (const constructor of [URL, URLSearchParams]) {
    const keys = ReflectOwnKeys(constructor.prototype);
    for (let i = 0; i < keys.length; i++) {
        const key = keys[i];
        if (key === "constructor") continue;
        if (key === SymbolToStringTag) {
            ObjectDefineProperty(constructor.prototype, key, { value: constructor.name, configurable: true });
        } else {
            const descriptor = ObjectGetOwnPropertyDescriptor(constructor.prototype, key);
            descriptor.enumerable = true;
            ObjectDefineProperty(constructor.prototype, key, descriptor);
        }
    }
}

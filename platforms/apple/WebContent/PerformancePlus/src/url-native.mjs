// Native parsing and form encoding only. Public URL/query policy is shared
// with V8, including methods missing from older WebKit URLSearchParams.
const NativeURL = globalThis.URL;
const NativeParams = globalThis.URLSearchParams;
const apply = Reflect.apply;
const descriptor = Object.getOwnPropertyDescriptor;
const fields = ["href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "hash"];
const getters = fields.map(name => descriptor(NativeURL.prototype, name).get);
const setters = fields.map(name => descriptor(NativeURL.prototype, name).set);
const append = NativeParams.prototype.append;
const entries = NativeParams.prototype.entries;
const serialize = NativeParams.prototype.toString;
const sampleIterator = apply(entries, new NativeParams(), []);
const next = Object.getPrototypeOf(sampleIterator).next;
const wellFormed = String.prototype.toWellFormed;

export function op_url_parse(input, base) { return new NativeURL(input, base); }
export function op_url_parts(url) {
  const parts = [];
  for (let i = 0; i < getters.length; i++) parts[i] = apply(getters[i], url, []);
  return parts;
}
export function op_url_set(url, field, value) {
  apply(setters[field], url, [value]);
  return op_url_parts(url);
}
export function op_url_search_parse(query) {
  const pairs = [], iterator = apply(entries, new NativeParams(query), []);
  for (;;) {
    const item = apply(next, iterator, []);
    if (item.done) return pairs;
    pairs[pairs.length] = item.value;
  }
}
export function op_url_search_serialize(pairs) {
  const params = new NativeParams();
  for (let i = 0; i < pairs.length; i++) apply(append, params, [pairs[i][0], pairs[i][1]]);
  return apply(serialize, params, []);
}
export function toWellFormed(value) {
  if (wellFormed) return apply(wellFormed, value, []);
  // iOS 15 lacks String#toWellFormed. Reuse WebIDL's native USVString
  // conversion instead of supplying another UTF-16 decoder/polyfill.
  const params = new NativeParams();
  apply(append, params, [value, ""]);
  return apply(next, apply(entries, params, []), []).value[0];
}

// Native URL ownership is held by core cppgc. WebKit substitutes this backend;
// the URL facade and live query-list policy remain the same modules.
import { primordials } from "ext:core/mod.js";
import { op_url_parse, op_url_parts, op_url_set, op_url_search_parse, op_url_search_serialize } from "ext:core/ops";
const { StringPrototypeToWellFormed } = primordials;
const toWellFormed = StringPrototypeToWellFormed;
export { op_url_parse, op_url_parts, op_url_set, op_url_search_parse, op_url_search_serialize, toWellFormed };

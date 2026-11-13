// The Host Kit's JSON: strict reading, compact writing, and what the services
// read requests with.

#include <migo/windows/json.hpp>

#include <cstdio>
#include <string>

namespace {

int g_failures = 0;

#define CHECK(condition)                                                                        \
    do {                                                                                        \
        if (!(condition)) {                                                                     \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #condition); \
            ++g_failures;                                                                       \
        }                                                                                       \
    } while (false)

using migo::windows_host::Json;

bool refused(const char *text) { return !Json::parse(text).has_value(); }

std::string round(const char *text) {
    const auto parsed = Json::parse(text);
    return parsed.has_value() ? parsed->dump() : std::string("<refused>");
}

void values_read_and_write_back() {
    CHECK(round("null") == "null");
    CHECK(round(" true ") == "true");
    CHECK(round("[1,2.5,-3e2,0]") == "[1,2.5,-300,0]");
    CHECK(round("{\"a\":{\"b\":[\"c\"]},\"d\":false}") == "{\"a\":{\"b\":[\"c\"]},\"d\":false}");
    CHECK(round("\"\\u00e9\\ud83d\\ude00\\n\\\"\"") == "\"\xC3\xA9\xF0\x9F\x98\x80\\n\\\"\"");
    CHECK(round("\"\xE4\xBD\xA0\"") == "\"\xE4\xBD\xA0\"");
    CHECK(round("1e21") == "1e+21");
    CHECK(round("9007199254740993") == "9007199254740992");
    CHECK(round("0.1") == "0.1");
    // A repeated name keeps its last value.
    CHECK(round("{\"a\":1,\"a\":2}") == "{\"a\":2}");
    CHECK(Json(std::string("\x01")).dump() == "\"\\u0001\"");
}

void what_rfc_8259_refuses_is_refused() {
    CHECK(refused(""));
    CHECK(refused("[1,]"));
    CHECK(refused("{\"a\":1,}"));
    CHECK(refused("// x\n1"));
    CHECK(refused("NaN"));
    CHECK(refused("01"));
    CHECK(refused("1."));
    CHECK(refused(".5"));
    CHECK(refused("1e999"));
    CHECK(refused("\"\\ud800\""));
    CHECK(refused("\"\\udc00\""));
    CHECK(refused("\"\x80\""));
    CHECK(refused("\"\xC0\xAF\""));
    CHECK(refused("\"\xED\xA0\x80\""));
    CHECK(refused("\"a\nb\""));
    CHECK(refused("{a:1}"));
    CHECK(refused("[1] 2"));
    std::string deep(300, '[');
    deep += std::string(300, ']');
    CHECK(refused(deep.c_str()));
    std::string shallow(200, '[');
    shallow += std::string(200, ']');
    CHECK(!refused(shallow.c_str()));
}

void requests_are_read_with_fallbacks() {
    const Json request = *Json::parse("{\"videoId\":7,\"src\":\"a.mp4\",\"loop\":true,\"x\":1.5}");
    CHECK(request["videoId"].numberOr(0) == 7);
    CHECK(request["src"].string() == "a.mp4");
    CHECK(request["loop"].boolOr(false));
    CHECK(request["missing"].isNull());
    CHECK(request["missing"]["deeper"].isNull());
    CHECK(request["src"].numberOr(-1) == -1);
    CHECK(request["x"].stringOr("none") == "none");
    CHECK(request.contains("loop") && !request.contains("muted"));
    Json answer;
    answer.set("data", "x").set("count", 2).set("data", "y");
    CHECK(answer.dump() == "{\"data\":\"y\",\"count\":2}");
    Json list;
    list.push(1).push("two");
    CHECK(list.dump() == "[1,\"two\"]");
    CHECK(*Json::parse(answer.dump()) == answer);
}

}  // namespace

int main() {
    values_read_and_write_back();
    what_rfc_8259_refuses_is_refused();
    requests_are_read_with_fallbacks();
    if (g_failures != 0) {
        std::fprintf(stderr, "%d Json assertion(s) failed\n", g_failures);
        return 1;
    }
    std::puts("Json contract: PASS");
    return 0;
}

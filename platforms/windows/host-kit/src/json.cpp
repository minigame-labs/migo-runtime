#include <migo/windows/json.hpp>

#include <charconv>
#include <clocale>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <locale.h>

namespace migo::windows_host {

namespace {

constexpr int kMaxDepth = 256;
// Whole numbers JavaScript holds exactly.
constexpr double kExactInteger = 9007199254740992.0;

const Json &null_json() noexcept {
    static const Json value;
    return value;
}

const std::string &empty_string() noexcept {
    static const std::string value;
    return value;
}

const Json::Array &empty_array() noexcept {
    static const Json::Array value;
    return value;
}

const Json::Object &empty_object() noexcept {
    static const Json::Object value;
    return value;
}

/// A strict RFC 8259 reader over one text.
class Reader {
public:
    explicit Reader(std::string_view text) : text_(text) {}

    std::optional<Json> document() {
        std::optional<Json> value = read(0);
        skipSpace();
        if (!value.has_value() || at_ != text_.size()) return std::nullopt;
        return value;
    }

private:
    void skipSpace() noexcept {
        while (at_ < text_.size() &&
               (text_[at_] == ' ' || text_[at_] == '\t' || text_[at_] == '\n' || text_[at_] == '\r')) {
            ++at_;
        }
    }

    bool literal(std::string_view word) noexcept {
        if (text_.substr(at_, word.size()) != word) return false;
        at_ += word.size();
        return true;
    }

    std::optional<Json> read(int depth) {
        if (depth > kMaxDepth) return std::nullopt;
        skipSpace();
        if (at_ >= text_.size()) return std::nullopt;
        switch (text_[at_]) {
            case 'n':
                return literal("null") ? std::optional<Json>(Json()) : std::nullopt;
            case 't':
                return literal("true") ? std::optional<Json>(Json(true)) : std::nullopt;
            case 'f':
                return literal("false") ? std::optional<Json>(Json(false)) : std::nullopt;
            case '"': {
                std::string value;
                if (!string(value)) return std::nullopt;
                return Json(std::move(value));
            }
            case '[':
                return array(depth);
            case '{':
                return object(depth);
            default:
                return number();
        }
    }

    std::optional<Json> array(int depth) {
        ++at_;
        Json::Array items;
        skipSpace();
        if (at_ < text_.size() && text_[at_] == ']') {
            ++at_;
            return Json(std::move(items));
        }
        while (true) {
            std::optional<Json> item = read(depth + 1);
            if (!item.has_value()) return std::nullopt;
            items.push_back(std::move(*item));
            skipSpace();
            if (at_ >= text_.size()) return std::nullopt;
            if (text_[at_] == ',') {
                ++at_;
                continue;
            }
            if (text_[at_] != ']') return std::nullopt;
            ++at_;
            return Json(std::move(items));
        }
    }

    std::optional<Json> object(int depth) {
        ++at_;
        Json value = Json::object();
        skipSpace();
        if (at_ < text_.size() && text_[at_] == '}') {
            ++at_;
            return value;
        }
        while (true) {
            skipSpace();
            std::string key;
            if (at_ >= text_.size() || text_[at_] != '"' || !string(key)) return std::nullopt;
            skipSpace();
            if (at_ >= text_.size() || text_[at_] != ':') return std::nullopt;
            ++at_;
            std::optional<Json> member = read(depth + 1);
            if (!member.has_value()) return std::nullopt;
            // A repeated name keeps its last value, as JSON.parse does.
            value.set(key, std::move(*member));
            skipSpace();
            if (at_ >= text_.size()) return std::nullopt;
            if (text_[at_] == ',') {
                ++at_;
                continue;
            }
            if (text_[at_] != '}') return std::nullopt;
            ++at_;
            return value;
        }
    }

    static void append_utf8(std::string &out, std::uint32_t code) {
        if (code < 0x80) {
            out.push_back(static_cast<char>(code));
        } else if (code < 0x800) {
            out.push_back(static_cast<char>(0xC0 | (code >> 6)));
            out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
        } else if (code < 0x10000) {
            out.push_back(static_cast<char>(0xE0 | (code >> 12)));
            out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
        } else {
            out.push_back(static_cast<char>(0xF0 | (code >> 18)));
            out.push_back(static_cast<char>(0x80 | ((code >> 12) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
        }
    }

    bool hex4(std::uint32_t &code) noexcept {
        if (at_ + 4 > text_.size()) return false;
        code = 0;
        for (int index = 0; index < 4; ++index) {
            const char digit = text_[at_++];
            code <<= 4;
            if (digit >= '0' && digit <= '9') {
                code |= static_cast<std::uint32_t>(digit - '0');
            } else if (digit >= 'a' && digit <= 'f') {
                code |= static_cast<std::uint32_t>(digit - 'a' + 10);
            } else if (digit >= 'A' && digit <= 'F') {
                code |= static_cast<std::uint32_t>(digit - 'A' + 10);
            } else {
                return false;
            }
        }
        return true;
    }

    /// One UTF-8 sequence starting at `at_`, copied whole when it is valid.
    bool utf8(std::string &out) {
        const auto lead = static_cast<unsigned char>(text_[at_]);
        std::size_t length = 0;
        std::uint32_t code = 0;
        if (lead >= 0xC2 && lead <= 0xDF) {
            length = 2;
            code = lead & 0x1F;
        } else if (lead >= 0xE0 && lead <= 0xEF) {
            length = 3;
            code = lead & 0x0F;
        } else if (lead >= 0xF0 && lead <= 0xF4) {
            length = 4;
            code = lead & 0x07;
        } else {
            return false;
        }
        if (at_ + length > text_.size()) return false;
        for (std::size_t index = 1; index < length; ++index) {
            const auto next = static_cast<unsigned char>(text_[at_ + index]);
            if ((next & 0xC0) != 0x80) return false;
            code = (code << 6) | (next & 0x3F);
        }
        // Overlong forms, surrogates and code points past Unicode's.
        if ((length == 3 && code < 0x800) || (length == 4 && (code < 0x10000 || code > 0x10FFFF)) ||
            (code >= 0xD800 && code <= 0xDFFF)) {
            return false;
        }
        out.append(text_.data() + at_, length);
        at_ += length;
        return true;
    }

    bool string(std::string &out) {
        ++at_;
        while (at_ < text_.size()) {
            const char c = text_[at_];
            if (c == '"') {
                ++at_;
                return true;
            }
            if (static_cast<unsigned char>(c) < 0x20) return false;
            if (c == '\\') {
                if (++at_ >= text_.size()) return false;
                const char escape = text_[at_++];
                switch (escape) {
                    case '"':
                        out.push_back('"');
                        break;
                    case '\\':
                        out.push_back('\\');
                        break;
                    case '/':
                        out.push_back('/');
                        break;
                    case 'b':
                        out.push_back('\b');
                        break;
                    case 'f':
                        out.push_back('\f');
                        break;
                    case 'n':
                        out.push_back('\n');
                        break;
                    case 'r':
                        out.push_back('\r');
                        break;
                    case 't':
                        out.push_back('\t');
                        break;
                    case 'u': {
                        std::uint32_t code = 0;
                        if (!hex4(code)) return false;
                        if (code >= 0xD800 && code <= 0xDBFF) {
                            // A pair's high half, which only its low half completes.
                            std::uint32_t low = 0;
                            if (!literal("\\u") || !hex4(low) || low < 0xDC00 || low > 0xDFFF) return false;
                            code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                        } else if (code >= 0xDC00 && code <= 0xDFFF) {
                            return false;
                        }
                        append_utf8(out, code);
                        break;
                    }
                    default:
                        return false;
                }
                continue;
            }
            if (static_cast<unsigned char>(c) < 0x80) {
                out.push_back(c);
                ++at_;
            } else if (!utf8(out)) {
                return false;
            }
        }
        return false;
    }

    std::optional<Json> number() {
        const std::size_t start = at_;
        if (at_ < text_.size() && text_[at_] == '-') ++at_;
        if (at_ >= text_.size()) return std::nullopt;
        if (text_[at_] == '0') {
            ++at_;
        } else if (text_[at_] >= '1' && text_[at_] <= '9') {
            while (at_ < text_.size() && text_[at_] >= '0' && text_[at_] <= '9') ++at_;
        } else {
            return std::nullopt;
        }
        if (at_ < text_.size() && text_[at_] == '.') {
            ++at_;
            const std::size_t digits = at_;
            while (at_ < text_.size() && text_[at_] >= '0' && text_[at_] <= '9') ++at_;
            if (at_ == digits) return std::nullopt;
        }
        if (at_ < text_.size() && (text_[at_] == 'e' || text_[at_] == 'E')) {
            ++at_;
            if (at_ < text_.size() && (text_[at_] == '+' || text_[at_] == '-')) ++at_;
            const std::size_t digits = at_;
            while (at_ < text_.size() && text_[at_] >= '0' && text_[at_] <= '9') ++at_;
            if (at_ == digits) return std::nullopt;
        }
        // The C runtime's reading, correctly rounded, in the "C" locale -- never
        // the thread's, whose decimal point may be a comma.
        static const _locale_t c_locale = _create_locale(LC_NUMERIC, "C");
        const std::string digits(text_.substr(start, at_ - start));
        char *end = nullptr;
        const double value = _strtod_l(digits.c_str(), &end, c_locale);
        // Too large for a double is infinity in JavaScript, which JSON cannot
        // say back; it is refused rather than changed.
        if (end != digits.c_str() + digits.size() || !std::isfinite(value)) return std::nullopt;
        return Json(value);
    }

    std::string_view text_;
    std::size_t at_ = 0;
};

void write_string(std::string &out, const std::string &value) {
    out.push_back('"');
    for (const char c : value) {
        switch (c) {
            case '"':
                out += "\\\"";
                break;
            case '\\':
                out += "\\\\";
                break;
            case '\b':
                out += "\\b";
                break;
            case '\f':
                out += "\\f";
                break;
            case '\n':
                out += "\\n";
                break;
            case '\r':
                out += "\\r";
                break;
            case '\t':
                out += "\\t";
                break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) {
                    char escaped[8];
                    std::snprintf(escaped, sizeof escaped, "\\u%04x", static_cast<unsigned>(c));
                    out += escaped;
                } else {
                    out.push_back(c);
                }
        }
    }
    out.push_back('"');
}

void write_number(std::string &out, double value) {
    if (!std::isfinite(value)) {
        out += "null";
        return;
    }
    char digits[32];
    if (std::fabs(value) < kExactInteger && value == std::floor(value)) {
        const auto whole = static_cast<long long>(value);
        const auto written = std::to_chars(digits, digits + sizeof digits, whole);
        out.append(digits, written.ptr);
        return;
    }
    // The shortest text that reads back as the same double.
    const auto written = std::to_chars(digits, digits + sizeof digits, value);
    out.append(digits, written.ptr);
}

}  // namespace

std::optional<Json> Json::parse(std::string_view text) { return Reader(text).document(); }

std::string Json::dump() const {
    std::string out;
    write(out);
    return out;
}

void Json::write(std::string &out) const {
    switch (type()) {
        case Type::Null:
            out += "null";
            return;
        case Type::Bool:
            out += std::get<bool>(value_) ? "true" : "false";
            return;
        case Type::Number:
            write_number(out, std::get<double>(value_));
            return;
        case Type::String:
            write_string(out, std::get<std::string>(value_));
            return;
        case Type::Array: {
            out.push_back('[');
            bool first = true;
            for (const Json &item : std::get<Array>(value_)) {
                if (!first) out.push_back(',');
                first = false;
                item.write(out);
            }
            out.push_back(']');
            return;
        }
        case Type::Object: {
            out.push_back('{');
            bool first = true;
            for (const Member &member : std::get<Object>(value_)) {
                if (!first) out.push_back(',');
                first = false;
                write_string(out, member.first);
                out.push_back(':');
                member.second.write(out);
            }
            out.push_back('}');
            return;
        }
    }
}

bool Json::boolOr(bool fallback) const noexcept { return isBool() ? std::get<bool>(value_) : fallback; }

double Json::numberOr(double fallback) const noexcept { return isNumber() ? std::get<double>(value_) : fallback; }

std::string Json::stringOr(std::string fallback) const {
    return isString() ? std::get<std::string>(value_) : std::move(fallback);
}

const std::string &Json::string() const noexcept {
    return isString() ? std::get<std::string>(value_) : empty_string();
}

const Json::Array &Json::items() const noexcept { return isArray() ? std::get<Array>(value_) : empty_array(); }

const Json::Object &Json::members() const noexcept { return isObject() ? std::get<Object>(value_) : empty_object(); }

const Json &Json::operator[](std::string_view key) const noexcept {
    for (const Member &member : members()) {
        if (member.first == key) return member.second;
    }
    return null_json();
}

bool Json::contains(std::string_view key) const noexcept {
    for (const Member &member : members()) {
        if (member.first == key) return true;
    }
    return false;
}

Json &Json::set(std::string_view key, Json value) {
    if (isNull()) value_ = Object{};
    if (!isObject()) return *this;
    Object &object = std::get<Object>(value_);
    for (Member &member : object) {
        if (member.first == key) {
            member.second = std::move(value);
            return *this;
        }
    }
    object.emplace_back(std::string(key), std::move(value));
    return *this;
}

Json &Json::push(Json value) {
    if (isNull()) value_ = Array{};
    if (isArray()) std::get<Array>(value_).push_back(std::move(value));
    return *this;
}

bool operator==(const Json &left, const Json &right) noexcept { return left.value_ == right.value_; }

}  // namespace migo::windows_host

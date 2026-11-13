#ifndef MIGO_WINDOWS_JSON_HPP_
#define MIGO_WINDOWS_JSON_HPP_

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

namespace migo::windows_host {

/// A JSON value: what a host service's requests, answers and events are made
/// of over the C ABI, which carries them as UTF-8 JSON text.
///
/// The Windows Host Kit's own -- a Win32 App has no JSON library it can be
/// assumed to have -- and only as much as the services need: the six kinds of
/// value, a strict parser (RFC 8259: no comments, no trailing commas, no NaN,
/// valid UTF-8, nested at most 256 deep), and compact output. Numbers are
/// doubles, as JavaScript's are; one that is a whole number within 2^53 is
/// written without a fraction, and one that is not finite as null. An object
/// keeps its members in the order they were added -- the requests are small,
/// so a member is found by looking.
class Json {
public:
    enum class Type : std::uint8_t { Null, Bool, Number, String, Array, Object };
    using Array = std::vector<Json>;
    using Member = std::pair<std::string, Json>;
    using Object = std::vector<Member>;

    Json() noexcept = default;
    Json(std::nullptr_t) noexcept {}
    Json(bool value) noexcept : value_(value) {}
    Json(double value) noexcept : value_(value) {}
    Json(int value) noexcept : value_(static_cast<double>(value)) {}
    Json(std::int64_t value) noexcept : value_(static_cast<double>(value)) {}
    Json(std::uint32_t value) noexcept : value_(static_cast<double>(value)) {}
    Json(std::uint64_t value) noexcept : value_(static_cast<double>(value)) {}
    Json(std::string value) : value_(std::move(value)) {}
    Json(std::string_view value) : value_(std::string(value)) {}
    Json(const char *value) : value_(std::string(value)) {}
    Json(Array value) : value_(std::move(value)) {}
    Json(Object value) : value_(std::move(value)) {}

    /// An empty object, the answer most calls give.
    [[nodiscard]] static Json object() { return Json(Object{}); }

    /// The value `text` holds, or none when it is not exactly one JSON value.
    [[nodiscard]] static std::optional<Json> parse(std::string_view text);
    /// The value as compact JSON text.
    [[nodiscard]] std::string dump() const;

    [[nodiscard]] Type type() const noexcept { return static_cast<Type>(value_.index()); }
    [[nodiscard]] bool isNull() const noexcept { return type() == Type::Null; }
    [[nodiscard]] bool isBool() const noexcept { return type() == Type::Bool; }
    [[nodiscard]] bool isNumber() const noexcept { return type() == Type::Number; }
    [[nodiscard]] bool isString() const noexcept { return type() == Type::String; }
    [[nodiscard]] bool isArray() const noexcept { return type() == Type::Array; }
    [[nodiscard]] bool isObject() const noexcept { return type() == Type::Object; }

    /// The value as one kind, or `fallback` when it is another.
    [[nodiscard]] bool boolOr(bool fallback) const noexcept;
    [[nodiscard]] double numberOr(double fallback) const noexcept;
    [[nodiscard]] std::string stringOr(std::string fallback) const;
    /// The string, or an empty one when the value is not a string.
    [[nodiscard]] const std::string &string() const noexcept;
    /// The elements, or none when the value is not an array.
    [[nodiscard]] const Array &items() const noexcept;
    /// The members, or none when the value is not an object.
    [[nodiscard]] const Object &members() const noexcept;

    /// An object's member `key`, or null when it has none -- or is not an object.
    [[nodiscard]] const Json &operator[](std::string_view key) const noexcept;
    [[nodiscard]] bool contains(std::string_view key) const noexcept;
    /// Sets the member `key` -- replacing one of that name -- making the value
    /// an object first when it is null. Returns the value, for chaining.
    Json &set(std::string_view key, Json value);
    /// Appends an element, making the value an array first when it is null.
    Json &push(Json value);

    friend bool operator==(const Json &left, const Json &right) noexcept;
    friend bool operator!=(const Json &left, const Json &right) noexcept { return !(left == right); }

private:
    void write(std::string &out) const;
    std::variant<std::nullptr_t, bool, double, std::string, Array, Object> value_;
};

}  // namespace migo::windows_host

#endif  // MIGO_WINDOWS_JSON_HPP_

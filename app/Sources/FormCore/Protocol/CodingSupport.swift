import Foundation

/// A coding key whose name is only known at runtime — used to flatten a struct into its
/// parent's container and to carry through keys this build does not know about.
struct DynamicKey: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }
    init(_ stringValue: String) { self.stringValue = stringValue }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

/// Keys present on the wire that this build has no field for.
///
/// The protocol is additive (spec 00 §1.6): a core newer than the app may send fields we do
/// not model. For documents the app sends *back* — `Settings` above all — dropping them
/// would silently delete a setting the user set in a newer build, so they are captured here
/// and re-encoded verbatim.
func decodeUnknownKeys(from decoder: Decoder, known: Set<String>) throws -> [String: JSONValue] {
    guard case let .object(all) = try JSONValue(from: decoder) else { return [:] }
    return all.filter { !known.contains($0.key) }
}

func encodeUnknownKeys(_ extra: [String: JSONValue], to encoder: Encoder) throws {
    guard !extra.isEmpty else { return }
    var c = encoder.container(keyedBy: DynamicKey.self)
    for (key, value) in extra {
        try c.encode(value, forKey: DynamicKey(key))
    }
}

// MARK: - Open string enums

/// A string enum that does not fail on a value it has never heard of.
///
/// The ladders on this boundary grow (`ThinkingLevel`, `StopReason`, …). A `String`-backed
/// Swift enum would throw on a new one and take the whole event with it, so these are
/// structs with known values as statics: unknown values decode, compare and re-encode
/// intact. Switches over them need a `default`, which is the correct thing to be forced into.
public protocol OpenStringValue:
    RawRepresentable, Codable, Sendable, Hashable, CustomStringConvertible, ExpressibleByStringLiteral
where RawValue == String {
    init(_ rawValue: String)
}

extension OpenStringValue {
    public init(rawValue: String) { self.init(rawValue) }
    public init(stringLiteral value: String) { self.init(value) }
    public init(from decoder: Decoder) throws {
        self.init(try decoder.singleValueContainer().decode(String.self))
    }
    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(rawValue)
    }
    public var description: String { rawValue }
}

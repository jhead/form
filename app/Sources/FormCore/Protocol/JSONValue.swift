import Foundation

/// A whole JSON document as a value type.
///
/// The protocol carries a few genuinely open payloads — tool arguments and results, the
/// `detail` on an error, the raw body of an event or block this build does not know about —
/// and this is how they survive a decode/encode round trip without being flattened to
/// `Any`. It is `Sendable` and `Equatable`, which `Any` is not.
public enum JSONValue: Sendable, Hashable, Codable {
    case null
    case bool(Bool)
    /// Kept distinct from `.double` so integers round-trip as integers.
    case int(Int64)
    case double(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() {
            self = .null
        } else if let v = try? c.decode(Bool.self) {
            self = .bool(v)
        } else if let v = try? c.decode(Int64.self) {
            self = .int(v)
        } else if let v = try? c.decode(Double.self) {
            self = .double(v)
        } else if let v = try? c.decode(String.self) {
            self = .string(v)
        } else if let v = try? c.decode([JSONValue].self) {
            self = .array(v)
        } else if let v = try? c.decode([String: JSONValue].self) {
            self = .object(v)
        } else {
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "not JSON")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case let .bool(v): try c.encode(v)
        case let .int(v): try c.encode(v)
        case let .double(v): try c.encode(v)
        case let .string(v): try c.encode(v)
        case let .array(v): try c.encode(v)
        case let .object(v): try c.encode(v)
        }
    }

    // Numbers compare numerically: `1` and `1.0` are the same JSON value, and a Rust `u64`
    // that Swift re-encodes as a `Double` must not read as drift.
    public static func == (lhs: JSONValue, rhs: JSONValue) -> Bool {
        switch (lhs, rhs) {
        case (.null, .null): true
        case let (.bool(a), .bool(b)): a == b
        case let (.string(a), .string(b)): a == b
        case let (.array(a), .array(b)): a == b
        case let (.object(a), .object(b)): a == b
        case let (.int(a), .int(b)): a == b
        case let (.double(a), .double(b)): a == b
        case let (.int(a), .double(b)): Double(a) == b
        case let (.double(a), .int(b)): a == Double(b)
        default: false
        }
    }

    public func hash(into hasher: inout Hasher) {
        switch self {
        case .null: hasher.combine(0)
        case let .bool(v): hasher.combine(v)
        case let .int(v): hasher.combine(Double(v))
        case let .double(v): hasher.combine(v)
        case let .string(v): hasher.combine(v)
        case let .array(v): hasher.combine(v)
        case let .object(v): hasher.combine(v)
        }
    }
}

// MARK: - Accessors

extension JSONValue {
    public subscript(key: String) -> JSONValue? {
        if case let .object(o) = self { return o[key] }
        return nil
    }

    public subscript(index: Int) -> JSONValue? {
        if case let .array(a) = self, a.indices.contains(index) { return a[index] }
        return nil
    }

    public var isNull: Bool { self == .null }
    public var stringValue: String? { if case let .string(v) = self { return v } else { return nil } }
    public var boolValue: Bool? { if case let .bool(v) = self { return v } else { return nil } }
    public var objectValue: [String: JSONValue]? {
        if case let .object(v) = self { return v } else { return nil }
    }
    public var arrayValue: [JSONValue]? {
        if case let .array(v) = self { return v } else { return nil }
    }

    public var intValue: Int64? {
        switch self {
        case let .int(v): v
        case let .double(v): Int64(exactly: v.rounded())
        default: nil
        }
    }

    public var doubleValue: Double? {
        switch self {
        case let .int(v): Double(v)
        case let .double(v): v
        default: nil
        }
    }
}

// MARK: - Conversions

extension JSONValue {
    public init(data: Data) throws {
        self = try JSONDecoder().decode(JSONValue.self, from: data)
    }

    public init(jsonString: String) throws {
        try self.init(data: Data(jsonString.utf8))
    }

    public func encoded(sortedKeys: Bool = true) throws -> Data {
        let encoder = JSONEncoder()
        if sortedKeys { encoder.outputFormatting = [.sortedKeys] }
        return try encoder.encode(self)
    }

    /// Sorted-key text, for diff output in test failures.
    public var canonicalString: String {
        (try? encoded()).map { String(decoding: $0, as: UTF8.self) } ?? "<unencodable>"
    }

    /// Drops explicitly-null members recursively.
    ///
    /// Spec 00 §1.5 treats an absent optional and a `null` one as the same thing, and the two
    /// sides disagree in practice — Rust omits some `Option`s and writes `null` for others.
    /// Normalizing here is what keeps the drift test measuring real drift.
    public var normalized: JSONValue {
        switch self {
        case let .array(a): .array(a.map(\.normalized))
        case let .object(o):
            .object(o.compactMapValues { $0.isNull ? nil : $0.normalized })
        default: self
        }
    }
}

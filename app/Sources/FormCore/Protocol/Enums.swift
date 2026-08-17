import Foundation

/// `pi`'s reasoning ladder, exactly (spec 04 §1). A model with no reasoning capability
/// lists only `.off`.
public struct ThinkingLevel: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let off = ThinkingLevel("off")
    public static let minimal = ThinkingLevel("minimal")
    public static let low = ThinkingLevel("low")
    public static let medium = ThinkingLevel("medium")
    public static let high = ThinkingLevel("high")
    public static let xhigh = ThinkingLevel("xhigh")
    public static let max = ThinkingLevel("max")

    /// Ladder order, for pickers. A value outside this list still decodes.
    public static let ladder: [ThinkingLevel] = [
        .off, .minimal, .low, .medium, .high, .xhigh, .max,
    ]

    /// Display name for the composer's model chip (F8.3).
    public var displayName: String {
        switch self {
        case .off: "Off"
        case .minimal: "Minimal"
        case .low: "Low"
        case .medium: "Medium"
        case .high: "High"
        case .xhigh: "Extra High"
        case .max: "Max"
        default: rawValue.capitalized
        }
    }
}

public struct SessionStatus: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let idle = SessionStatus("idle")
    public static let streaming = SessionStatus("streaming")
    public static let error = SessionStatus("error")
}

public struct StopReason: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let pending = StopReason("pending")
    public static let stop = StopReason("stop")
    public static let length = StopReason("length")
    public static let toolUse = StopReason("toolUse")
    public static let error = StopReason("error")
    public static let aborted = StopReason("aborted")
    public static let deferred = StopReason("deferred")
}

public struct DoneReason: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let stop = DoneReason("stop")
    public static let length = DoneReason("length")
    public static let toolUse = DoneReason("toolUse")
    public static let deferred = DoneReason("deferred")
}

public struct ErrorReason: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let aborted = ErrorReason("aborted")
    public static let error = ErrorReason("error")
}

public struct RunOutcome: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let completed = RunOutcome("completed")
    public static let aborted = RunOutcome("aborted")
    public static let failed = RunOutcome("failed")
}

public struct SegmentKind: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let system = SegmentKind("system")
    public static let tools = SegmentKind("tools")
    public static let transcript = SegmentKind("transcript")
    public static let attachments = SegmentKind("attachments")
    public static let outputReserve = SegmentKind("outputReserve")

    /// Ring order, outermost segment last (F10.1).
    public static let all: [SegmentKind] = [
        .system, .tools, .transcript, .attachments, .outputReserve,
    ]

    public var displayName: String {
        switch self {
        case .system: "System"
        case .tools: "Tools"
        case .transcript: "Transcript"
        case .attachments: "Attachments"
        case .outputReserve: "Output reserve"
        default: rawValue
        }
    }
}

public struct AuthMethod: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let apiKey = AuthMethod("apiKey")
    public static let oauth = AuthMethod("oAuth")
    public static let none = AuthMethod("none")
}

public struct ThemeMode: OpenStringValue {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }

    public static let light = ThemeMode("light")
    public static let dark = ThemeMode("dark")
    public static let system = ThemeMode("system")

    public static let all: [ThemeMode] = [.light, .dark, .system]
}

/// The dashboard's period selector (F11). Swift picks the value, so a closed enum is right.
public enum StatsRange: String, Codable, Sendable, Hashable, CaseIterable {
    case d7, d30, all

    public var displayName: String {
        switch self {
        case .d7: "7d"
        case .d30: "30d"
        case .all: "All"
        }
    }
}

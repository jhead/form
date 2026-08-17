import Foundation

/// The Home dashboard's document (spec 03). **One query, one document** — every number F11
/// renders is in here, and nothing is aggregated in Swift.
///
/// Two things are deliberate. Every field decodes with a default, because W3 is still
/// filling the document in and a half-built core must still render the empty state (F11.12)
/// rather than throw. And `encode` writes back the document the core sent, when there was
/// one: this type is read-only on the wire, so its job on the way out is fidelity, not
/// reconstruction — a section this build has not caught up to yet survives untouched.
public struct UsageStats: Codable, Sendable, Equatable {
    public var range: StatsRange?
    public var generatedAt: TimestampMs
    public var headline: Headline
    public var daily: [DailyBucket]
    /// Always 24 entries.
    public var hourly: [HourlyBucket]
    /// 7 × 24 tokens.
    public var weekdayHour: [[Int64]]
    public var heatmap: [HeatmapCell]
    public var models: [ModelStat]
    public var providers: [ProviderStat]
    public var tools: [ToolStat]
    public var sessionsTop: SessionLeaderboards
    public var cache: CacheStats
    public var cost: CostStats
    public var latency: [LatencyStat]

    /// The document as received, re-encoded verbatim. `.null` for a locally-built value.
    public var raw: JSONValue

    public init(
        range: StatsRange? = nil, generatedAt: TimestampMs = Date.nowMs,
        headline: Headline = Headline(), daily: [DailyBucket] = [],
        hourly: [HourlyBucket] = [], weekdayHour: [[Int64]] = [], heatmap: [HeatmapCell] = [],
        models: [ModelStat] = [], providers: [ProviderStat] = [], tools: [ToolStat] = [],
        sessionsTop: SessionLeaderboards = SessionLeaderboards(),
        cache: CacheStats = CacheStats(), cost: CostStats = CostStats(),
        latency: [LatencyStat] = [], raw: JSONValue = .null
    ) {
        self.range = range
        self.generatedAt = generatedAt
        self.headline = headline
        self.daily = daily
        self.hourly = hourly
        self.weekdayHour = weekdayHour
        self.heatmap = heatmap
        self.models = models
        self.providers = providers
        self.tools = tools
        self.sessionsTop = sessionsTop
        self.cache = cache
        self.cost = cost
        self.latency = latency
        self.raw = raw
    }

    private enum CodingKeys: String, CodingKey {
        case range, generatedAt, headline, daily, hourly, weekdayHour, heatmap, models
        case providers, tools, sessionsTop, cache, cost, latency
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)

        // A section whose shape this build has not caught up to degrades to empty and says
        // so in the log, rather than taking the whole dashboard down with it.
        func section<T: Decodable>(_ key: CodingKeys, _ fallback: T) -> T {
            do {
                return try c.decodeIfPresent(T.self, forKey: key) ?? fallback
            } catch {
                Log.core.error(
                    """
                    stats: '\(key.stringValue, privacy: .public)' does not match this build's \
                    mirror: \(String(describing: error), privacy: .public)
                    """)
                return fallback
            }
        }

        range = try? c.decodeIfPresent(StatsRange.self, forKey: .range)
        generatedAt = try c.decodeIfPresent(TimestampMs.self, forKey: .generatedAt) ?? 0
        headline = section(.headline, Headline())
        daily = section(.daily, [])
        hourly = section(.hourly, [])
        weekdayHour = section(.weekdayHour, [])
        heatmap = section(.heatmap, [])
        models = section(.models, [])
        providers = section(.providers, [])
        tools = section(.tools, [])
        sessionsTop = section(.sessionsTop, SessionLeaderboards())
        cache = section(.cache, CacheStats())
        cost = section(.cost, CostStats())
        latency = section(.latency, [])
        raw = try JSONValue(from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        if case .object = raw {
            try raw.encode(to: encoder)
            return
        }
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(range, forKey: .range)
        try c.encode(generatedAt, forKey: .generatedAt)
        try c.encode(headline, forKey: .headline)
        try c.encode(daily, forKey: .daily)
        try c.encode(hourly, forKey: .hourly)
        try c.encode(weekdayHour, forKey: .weekdayHour)
        try c.encode(heatmap, forKey: .heatmap)
        try c.encode(models, forKey: .models)
        try c.encode(providers, forKey: .providers)
        try c.encode(tools, forKey: .tools)
        try c.encode(sessionsTop, forKey: .sessionsTop)
        try c.encode(cache, forKey: .cache)
        try c.encode(cost, forKey: .cost)
        try c.encode(latency, forKey: .latency)
    }

    public var isEmpty: Bool { headline.turns == 0 && headline.messages == 0 }
}

public struct Headline: Codable, Sendable, Equatable {
    public var sessions: Int64 = 0
    public var messages: Int64 = 0
    public var turns: Int64 = 0
    public var totalTokens: Int64 = 0
    public var input: Int64 = 0
    public var output: Int64 = 0
    public var cacheRead: Int64 = 0
    public var cacheWrite: Int64 = 0
    public var reasoning: Int64 = 0
    public var activeDays: Int = 0
    public var currentStreak: Int = 0
    public var longestStreak: Int = 0
    public var peakHour: Int = 0
    public var favoriteModel: ModelRef?
    public var totalCost: Double = 0
    public var avgSessionTokens: Int64 = 0
    public var avgTurnDurationMs: Int64 = 0

    public init() {}

    private enum CodingKeys: String, CodingKey {
        case sessions, messages, turns, totalTokens, input, output, cacheRead, cacheWrite
        case reasoning, activeDays, currentStreak, longestStreak, peakHour, favoriteModel
        case totalCost, avgSessionTokens, avgTurnDurationMs
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sessions = try c.decodeIfPresent(Int64.self, forKey: .sessions) ?? 0
        messages = try c.decodeIfPresent(Int64.self, forKey: .messages) ?? 0
        turns = try c.decodeIfPresent(Int64.self, forKey: .turns) ?? 0
        totalTokens = try c.decodeIfPresent(Int64.self, forKey: .totalTokens) ?? 0
        input = try c.decodeIfPresent(Int64.self, forKey: .input) ?? 0
        output = try c.decodeIfPresent(Int64.self, forKey: .output) ?? 0
        cacheRead = try c.decodeIfPresent(Int64.self, forKey: .cacheRead) ?? 0
        cacheWrite = try c.decodeIfPresent(Int64.self, forKey: .cacheWrite) ?? 0
        reasoning = try c.decodeIfPresent(Int64.self, forKey: .reasoning) ?? 0
        activeDays = try c.decodeIfPresent(Int.self, forKey: .activeDays) ?? 0
        currentStreak = try c.decodeIfPresent(Int.self, forKey: .currentStreak) ?? 0
        longestStreak = try c.decodeIfPresent(Int.self, forKey: .longestStreak) ?? 0
        peakHour = try c.decodeIfPresent(Int.self, forKey: .peakHour) ?? 0
        favoriteModel = try c.decodeIfPresent(ModelRef.self, forKey: .favoriteModel)
        totalCost = try c.decodeIfPresent(Double.self, forKey: .totalCost) ?? 0
        avgSessionTokens = try c.decodeIfPresent(Int64.self, forKey: .avgSessionTokens) ?? 0
        avgTurnDurationMs = try c.decodeIfPresent(Int64.self, forKey: .avgTurnDurationMs) ?? 0
    }
}

public struct DailyBucket: Codable, Sendable, Equatable, Identifiable {
    /// `YYYY-MM-DD` in the caller's timezone.
    public var date: String = ""
    public var sessions: Int64 = 0
    public var messages: Int64 = 0
    public var turns: Int64 = 0
    public var input: Int64 = 0
    public var output: Int64 = 0
    public var cacheRead: Int64 = 0
    public var cacheWrite: Int64 = 0
    public var totalTokens: Int64 = 0
    public var cost: Double = 0
    public var durationMs: Int64 = 0

    public init(date: String = "") { self.date = date }

    public var id: String { date }
}

public struct HourlyBucket: Codable, Sendable, Equatable, Identifiable {
    public var hour: Int = 0
    public var totalTokens: Int64 = 0
    public var turns: Int64 = 0

    public init(hour: Int = 0) { self.hour = hour }

    public var id: Int { hour }
}

/// GitHub-style contribution cell. `level` is 0–4; level 0 is reserved for exactly zero
/// (spec 03 §3).
public struct HeatmapCell: Codable, Sendable, Equatable, Identifiable {
    public var date: String = ""
    public var tokens: Int64 = 0
    public var sessions: Int64 = 0
    public var level: Int = 0

    public init(date: String = "", tokens: Int64 = 0, sessions: Int64 = 0, level: Int = 0) {
        self.date = date
        self.tokens = tokens
        self.sessions = sessions
        self.level = level
    }

    public var id: String { date }
}

public struct ModelStat: Codable, Sendable, Equatable, Identifiable {
    public var model: ModelRef
    public var displayName: String = ""
    public var turns: Int64 = 0
    public var totalTokens: Int64 = 0
    /// Fraction of the range's tokens; the set sums to 1.0.
    public var share: Double = 0
    public var cost: Double = 0
    public var avgTtftMs: Int64 = 0
    public var avgOutputTps: Double = 0
    public var errorRate: Double = 0

    public init(model: ModelRef, displayName: String = "") {
        self.model = model
        self.displayName = displayName
    }

    public var id: String { model.slug }
}

public struct ProviderStat: Codable, Sendable, Equatable, Identifiable {
    public var providerId: String = ""
    public var name: String = ""
    public var turns: Int64 = 0
    public var totalTokens: Int64 = 0
    public var share: Double = 0
    public var cost: Double = 0

    public init(providerId: String = "", name: String = "") {
        self.providerId = providerId
        self.name = name
    }

    public var id: String { providerId }
}

public struct ToolStat: Codable, Sendable, Equatable, Identifiable {
    public var name: String = ""
    public var invocations: Int64 = 0
    public var successRate: Double = 0
    public var meanDurationMs: Int64 = 0

    public init(name: String = "") { self.name = name }

    public var id: String { name }
}

public struct SessionRank: Codable, Sendable, Equatable, Identifiable {
    public var sessionId: String = ""
    public var title: String = ""
    public var totalTokens: Int64 = 0
    public var durationMs: Int64 = 0
    public var turns: Int64 = 0

    public init(sessionId: String = "", title: String = "") {
        self.sessionId = sessionId
        self.title = title
    }

    public var id: String { sessionId }
}

public struct SessionLeaderboards: Codable, Sendable, Equatable {
    public var byTokens: [SessionRank] = []
    public var byDuration: [SessionRank] = []
    public var byTurns: [SessionRank] = []

    public init() {}
}

public struct CachePoint: Codable, Sendable, Equatable, Identifiable {
    public var date: String = ""
    public var read: Int64 = 0
    public var write: Int64 = 0

    public init(date: String = "") { self.date = date }

    public var id: String { date }
}

public struct CacheStats: Codable, Sendable, Equatable {
    public var read: Int64 = 0
    public var write: Int64 = 0
    public var hitRatio: Double = 0
    public var estimatedSavings: Double = 0
    public var daily: [CachePoint] = []

    public init() {}
}

public struct CostPoint: Codable, Sendable, Equatable, Identifiable {
    public var date: String = ""
    public var cost: Double = 0

    public init(date: String = "") { self.date = date }

    public var id: String { date }
}

/// Rust emits `(key, value)` tuples as two-element arrays; this decodes either that or an
/// object, so a later shape change on the core side does not take the dashboard down.
public struct KeyedCost<Key: Codable & Sendable & Equatable>: Codable, Sendable, Equatable {
    public var key: Key
    public var cost: Double

    public init(key: Key, cost: Double) {
        self.key = key
        self.cost = cost
    }

    private enum CodingKeys: String, CodingKey { case key, cost }

    public init(from decoder: Decoder) throws {
        if var unkeyed = try? decoder.unkeyedContainer() {
            key = try unkeyed.decode(Key.self)
            cost = try unkeyed.decode(Double.self)
        } else {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            key = try c.decode(Key.self, forKey: .key)
            cost = try c.decode(Double.self, forKey: .cost)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.unkeyedContainer()
        try c.encode(key)
        try c.encode(cost)
    }
}

public struct CostStats: Codable, Sendable, Equatable {
    public var total: Double = 0
    public var byDay: [CostPoint] = []
    public var byProvider: [KeyedCost<String>] = []
    public var byModel: [KeyedCost<ModelRef>] = []
    /// Mean daily cost over the trailing 14 days × 30; `0` with fewer than 3 active days.
    public var projectedMonthly: Double = 0

    public init() {}
}

public struct HistogramBin: Codable, Sendable, Equatable, Identifiable {
    public var lower: Double = 0
    public var upper: Double = 0
    public var count: Int64 = 0

    public init() {}

    public var id: Double { lower }
}

public struct LatencyStat: Codable, Sendable, Equatable, Identifiable {
    public var model: ModelRef
    public var ttftP50: Int64 = 0
    public var ttftP90: Int64 = 0
    public var ttftP99: Int64 = 0
    public var tpsP50: Double = 0
    public var tpsP90: Double = 0
    public var tpsP99: Double = 0
    public var histogram: [HistogramBin] = []
    public var samples: Int64 = 0

    public init(model: ModelRef) { self.model = model }

    public var id: String { model.slug }
}

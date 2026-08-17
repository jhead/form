import Foundation

/// One event of a recorded run, with the gap that preceded it.
public struct RecordedEvent: Sendable, Equatable {
    public var delayMs: Int
    public var event: CoreEvent

    public init(delayMs: Int, event: CoreEvent) {
        self.delayMs = delayMs
        self.event = event
    }
}

/// The demo data `MockTransport` serves.
///
/// This exists so **every SwiftUI preview in W8–W14 works with no Rust build** (spec 07 §6):
/// grouped sessions, a transcript with thinking, markdown, a tool call and a turn footer, a
/// populated settings document, the model catalog, and a dashboard's worth of statistics.
/// The numbers are deterministic — a preview that changes every time it re-renders is not a
/// preview.
public struct MockCorpus: Sendable {
    public var groups: [SessionGroup]
    public var sessions: [SessionSummary]
    public var transcripts: [String: [Entry]]
    public var settings: Settings
    public var catalog: Catalog
    public var stats: [StatsRange: UsageStats]
    public var contextUsage: [String: ContextUsage]
    public var workspaces: [Workspace]
    public var attachments: [String: Attachment]

    public static let demo = MockCorpus()

    /// The session the previews open on: grouped, titled, with a real transcript.
    public var primarySessionId: String { "ses_health_check" }

    public init() {
        let now: TimestampMs = 1_755_000_000_000  // fixed: previews must not drift by the hour
        let day: TimestampMs = 86_400_000

        catalog = MockCorpus.makeCatalog()
        let opus = ModelRef(
            providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)
        let sonnet = ModelRef(
            providerId: "anthropic", modelId: "claude-sonnet-5", thinkingLevel: .medium)
        let gpt = ModelRef(providerId: "openai", modelId: "gpt-5", thinkingLevel: .low)

        groups = [
            SessionGroup(id: "grp_work", name: "form", index: 0),
            SessionGroup(id: "grp_side", name: "Side quests", index: 1),
            SessionGroup(id: "grp_empty", name: "Archive", index: 2, collapsed: true),
        ]

        sessions = [
            SessionSummary(
                id: "ses_health_check", title: "Add a health check endpoint",
                groupId: "grp_work", index: 0, workspaceRoot: "/Users/jhead/dev/form",
                modelRef: opus, status: .idle, messageCount: 6, totalTokens: 18_420,
                createdAt: now - day, updatedAt: now - 1_800_000),
            SessionSummary(
                id: "ses_streaming", title: "Refactor the event bus",
                groupId: "grp_work", index: 1, workspaceRoot: "/Users/jhead/dev/form",
                modelRef: opus, status: .streaming, messageCount: 3, totalTokens: 9_120,
                createdAt: now - 2 * day, updatedAt: now - 60_000),
            SessionSummary(
                id: "ses_pinned", title: "Sidebar drag and drop", titleIsCustom: true,
                groupId: "grp_work", index: 2, modelRef: sonnet, status: .idle,
                messageCount: 22, totalTokens: 84_900, pinned: true,
                createdAt: now - 5 * day, updatedAt: now - 3 * 3_600_000),
            SessionSummary(
                id: "ses_error", title: "Why is the ring stuck at 90%?",
                groupId: "grp_side", index: 0, modelRef: gpt, status: .error,
                messageCount: 4, totalTokens: 3_210,
                createdAt: now - 3 * day, updatedAt: now - 2 * day),
            SessionSummary(
                id: "ses_loose", title: "Scratch: chrono-tz bucketing",
                index: 0, modelRef: sonnet, status: .idle, messageCount: 2,
                totalTokens: 1_180, createdAt: now - 6 * day, updatedAt: now - 4 * day),
            SessionSummary(
                id: "ses_archived", title: "Old provider catalog spike",
                groupId: "grp_side", index: 1, modelRef: gpt, status: .idle,
                messageCount: 9, totalTokens: 12_000, archived: true,
                createdAt: now - 20 * day, updatedAt: now - 18 * day),
        ]

        transcripts = [
            "ses_health_check": MockCorpus.transcript(
                sessionId: "ses_health_check", at: now - 1_800_000, model: opus),
            "ses_streaming": MockCorpus.streamingTranscript(
                sessionId: "ses_streaming", at: now - 60_000, model: opus),
        ]

        var settings = Settings()
        settings.appearance.themeMode = .system
        settings.defaults.modelRef = opus
        settings.providers = [
            "anthropic": ProviderSettings(enabled: true, hasKey: true),
            "openai": ProviderSettings(enabled: true, hasKey: false),
        ]
        self.settings = settings

        workspaces = [
            Workspace(path: "/Users/jhead/dev/form", lastUsed: now - 1_800_000),
            Workspace(path: "/Users/jhead/dev/pi-rs", lastUsed: now - 3 * day),
        ]

        attachments = [
            "att_shot": Attachment(
                id: "att_shot", sessionId: "ses_health_check",
                sha256: String(repeating: "a1", count: 32), filename: "sidebar.png",
                mime: "image/png", bytes: 284_910, width: 1280, height: 800,
                path: "/tmp/form-mock/sidebar.png", thumbPath: "/tmp/form-mock/sidebar-thumb.png",
                createdAt: now - day)
        ]

        contextUsage = [
            "ses_health_check": ContextUsage(
                sessionId: "ses_health_check", used: 41_820, total: 200_000,
                segments: [
                    ContextSegment(kind: .system, tokens: 1_850),
                    ContextSegment(kind: .tools, tokens: 4_100),
                    ContextSegment(kind: .transcript, tokens: 18_420),
                    ContextSegment(kind: .attachments, tokens: 1_450),
                    ContextSegment(kind: .outputReserve, tokens: 16_000),
                ],
                cost: Cost(
                    input: 0.21, output: 0.46, cacheRead: 0.02, cacheWrite: 0.05, total: 0.74),
                messageCount: 6),
            "ses_streaming": ContextUsage(
                sessionId: "ses_streaming", used: 154_300, total: 200_000,
                segments: [
                    ContextSegment(kind: .system, tokens: 1_850),
                    ContextSegment(kind: .tools, tokens: 4_100),
                    ContextSegment(kind: .transcript, tokens: 130_900),
                    ContextSegment(kind: .attachments, tokens: 1_450),
                    ContextSegment(kind: .outputReserve, tokens: 16_000),
                ],
                cost: Cost(
                    input: 1.02, output: 2.4, cacheRead: 0.12, cacheWrite: 0.3, total: 3.84),
                messageCount: 3),
        ]

        stats = [
            .d7: MockCorpus.makeStats(range: .d7, days: 7, now: now),
            .d30: MockCorpus.makeStats(range: .d30, days: 30, now: now),
            .all: MockCorpus.makeStats(range: .all, days: 90, now: now),
        ]
    }

    public func transcript(for sessionId: String) -> [Entry] { transcripts[sessionId] ?? [] }

    public func session(_ id: String) -> Session? {
        guard let summary = sessions.first(where: { $0.id == id }) else { return nil }
        return Session(summary: summary, entries: transcript(for: id))
    }

    public func list(includeArchived: Bool) -> SessionList {
        SessionList(
            groups: groups,
            sessions: sessions.filter { includeArchived || !$0.archived })
    }
}

// MARK: - Transcripts

extension MockCorpus {
    static func entry(
        _ id: String, _ sessionId: String, _ seq: Int64, _ timestamp: TimestampMs,
        _ message: Message
    ) -> Entry {
        Entry(
            id: id, sessionId: sessionId, seq: seq, parentId: seq == 0 ? nil : "\(id)-prev",
            timestamp: timestamp, kind: .message(message: message))
    }

    static let replyMarkdown = """
        I'll add the endpoint and wire it into the router.

        **Plan**

        1. Add `GET /healthz` returning `{"status":"ok"}`
        2. Register it before the auth middleware
        3. Cover it with a test

        ```rust
        async fn healthz() -> impl IntoResponse {
            Json(json!({ "status": "ok" }))
        }
        ```

        Let me read the router first.
        """

    static func assistantMessage(
        _ model: ModelRef, at timestamp: TimestampMs, content: [AssistantContent],
        stopReason: StopReason = .stop, usage: Usage = Usage()
    ) -> AssistantMessage {
        AssistantMessage(
            content: content, api: "anthropic-messages", provider: model.providerId,
            model: model.modelId, usage: usage, stopReason: stopReason, timestamp: timestamp)
    }

    static func transcript(sessionId: String, at now: TimestampMs, model: ModelRef) -> [Entry] {
        let usage = Usage(
            input: 1_200, output: 486, cacheRead: 900, cacheWrite: 120, totalTokens: 1_686,
            cost: Cost(
                input: 0.006, output: 0.012, cacheRead: 0.0005, cacheWrite: 0.0008,
                total: 0.0193))
        let toolCall = ToolCall(
            id: "toolu_read_router", name: "read",
            arguments: ["path": .string("src/router.rs")])

        return [
            entry(
                "ent_1", sessionId, 0, now - 600_000,
                .user(UserMessage(text: "Add a health check endpoint", timestamp: now - 600_000))),
            entry(
                "ent_2", sessionId, 1, now - 598_000,
                .assistant(
                    assistantMessage(
                        model, at: now - 598_000,
                        content: [
                            .thinking(
                                ThinkingContent(
                                    thinking:
                                        "The router is in src/router.rs. I should check how the "
                                        + "existing routes register middleware before inserting.")),
                            .text(TextContent(text: replyMarkdown)),
                            .toolCall(toolCall),
                        ],
                        stopReason: .toolUse, usage: usage))),
            entry(
                "ent_3", sessionId, 2, now - 596_000,
                .toolResult(
                    ToolResultMessage(
                        toolCallId: toolCall.id, toolName: "read",
                        content: [.text("read 268 lines")],
                        details: .object([
                            "linesAdded": .int(268), "linesRemoved": .int(0),
                        ]),
                        timestamp: now - 596_000))),
            entry(
                "ent_4", sessionId, 3, now - 590_000,
                .assistant(
                    assistantMessage(
                        model, at: now - 590_000,
                        content: [
                            .text(
                                TextContent(
                                    text:
                                        "Done — `/healthz` is registered ahead of the auth layer "
                                        + "and the test passes."))
                        ],
                        usage: usage))),
        ]
    }

    /// A transcript caught mid-stream: the last message is incomplete and has no stop reason.
    static func streamingTranscript(sessionId: String, at now: TimestampMs, model: ModelRef)
        -> [Entry]
    {
        [
            entry(
                "ent_s1", sessionId, 0, now - 40_000,
                .user(
                    UserMessage(
                        text: "Refactor the event bus so listeners are keyed by token",
                        timestamp: now - 40_000))),
            entry(
                "ent_s2", sessionId, 1, now - 38_000,
                .assistant(
                    assistantMessage(
                        model, at: now - 38_000,
                        content: [
                            .thinking(
                                ThinkingContent(thinking: "Listeners are stored in a Vec today")),
                            .text(TextContent(text: "Switching the listener list to a map keyed")),
                        ],
                        stopReason: .pending))),
        ]
    }
}

// MARK: - Catalog

extension MockCorpus {
    static func makeCatalog() -> Catalog {
        let full: [ThinkingLevel] = ThinkingLevel.ladder
        let caps = Capabilities(
            vision: true, tools: true, reasoning: true, caching: true, streaming: true)
        return Catalog(providers: [
            Provider(
                id: "anthropic", name: "Anthropic", baseUrl: "https://api.anthropic.com",
                auth: [.apiKey, .oauth], envVars: ["ANTHROPIC_API_KEY"],
                models: [
                    Model(
                        id: "claude-opus-5", name: "Opus 5", family: "claude",
                        contextWindow: 200_000, maxOutput: 64_000,
                        pricing: Pricing(input: 5, output: 25, cacheRead: 0.5, cacheWrite: 6.25),
                        capabilities: caps, thinkingLevels: full),
                    Model(
                        id: "claude-sonnet-5", name: "Sonnet 5", family: "claude",
                        contextWindow: 200_000, maxOutput: 64_000,
                        pricing: Pricing(input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75),
                        capabilities: caps, thinkingLevels: full),
                ]),
            Provider(
                id: "openai", name: "OpenAI", baseUrl: "https://api.openai.com/v1",
                auth: [.apiKey], envVars: ["OPENAI_API_KEY"],
                models: [
                    Model(
                        id: "gpt-5", name: "GPT-5", family: "gpt", contextWindow: 400_000,
                        maxOutput: 128_000,
                        pricing: Pricing(input: 1.25, output: 10, cacheRead: 0.125),
                        capabilities: caps, thinkingLevels: full)
                ]),
        ])
    }
}

// MARK: - Statistics

extension MockCorpus {
    /// A deterministic generator — the same dashboard every time the preview redraws.
    private struct Seeded {
        private var state: UInt64
        init(_ seed: UInt64) { state = seed }
        mutating func next(_ upper: UInt64) -> UInt64 {
            state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
            return (state >> 33) % Swift.max(1, upper)
        }
        mutating func double(_ upper: Double) -> Double {
            Double(next(1_000_000)) / 1_000_000 * upper
        }
    }

    static func makeStats(range: StatsRange, days: Int, now: TimestampMs) -> UsageStats {
        var rng = Seeded(UInt64(days) &* 7)
        var stats = UsageStats(range: range, generatedAt: now)

        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd"
        formatter.timeZone = TimeZone(identifier: "UTC")

        var daily: [DailyBucket] = []
        var heatmap: [HeatmapCell] = []
        var totalTokens: Int64 = 0
        var totalCost = 0.0
        var totalTurns: Int64 = 0
        var activeDays = 0

        for offset in stride(from: days - 1, through: 0, by: -1) {
            let date = formatter.string(
                from: Date(msSinceEpoch: now - Int64(offset) * 86_400_000))
            // Weekends are quiet, and one day in nine is empty — sparse states must be
            // exercisable (F11.12).
            let quiet = rng.next(9) == 0
            let turns = quiet ? 0 : Int64(4 + rng.next(26))
            let input = turns * Int64(900 + rng.next(2_400))
            let output = turns * Int64(180 + rng.next(900))
            let cacheRead = turns * Int64(rng.next(3_000))
            let cacheWrite = turns * Int64(rng.next(400))
            let tokens = input + output + cacheRead + cacheWrite
            let cost =
                Double(input) * 5e-6 + Double(output) * 25e-6 + Double(cacheRead) * 0.5e-6

            var bucket = DailyBucket(date: date)
            bucket.turns = turns
            bucket.sessions = turns == 0 ? 0 : Int64(1 + rng.next(3))
            bucket.messages = turns * 2
            bucket.input = input
            bucket.output = output
            bucket.cacheRead = cacheRead
            bucket.cacheWrite = cacheWrite
            bucket.totalTokens = tokens
            bucket.cost = cost
            bucket.durationMs = turns * Int64(8_000 + rng.next(40_000))
            daily.append(bucket)

            heatmap.append(
                HeatmapCell(
                    date: date, tokens: tokens, sessions: bucket.sessions,
                    level: tokens == 0 ? 0 : Int(1 + min(3, tokens / 40_000))))

            totalTokens += tokens
            totalCost += cost
            totalTurns += turns
            if turns > 0 { activeDays += 1 }
        }

        stats.daily = daily
        stats.heatmap = heatmap

        // Work happens in the afternoon and evening, which is what makes the histogram
        // worth rendering at all.
        let shape: [Double] = (0..<24).map { hour in
            switch hour {
            case 0..<7: 0.1
            case 7..<10: 0.5
            case 10..<13: 1.0
            case 13..<18: 1.3
            case 18..<22: 0.9
            default: 0.3
            }
        }
        stats.hourly = (0..<24).map { hour in
            var bucket = HourlyBucket(hour: hour)
            bucket.totalTokens = Int64(Double(totalTokens) / 24 * shape[hour])
            bucket.turns = Int64(Double(totalTurns) / 24 * shape[hour])
            return bucket
        }
        stats.weekdayHour = (0..<7).map { weekday in
            (0..<24).map { hour in
                let weekendDamping = (weekday == 0 || weekday == 6) ? 0.35 : 1.0
                return Int64(Double(totalTokens) / 168 * shape[hour] * weekendDamping)
            }
        }

        var headline = Headline()
        headline.sessions = Int64(daily.reduce(0) { $0 + $1.sessions })
        headline.turns = totalTurns
        headline.messages = totalTurns * 2
        headline.totalTokens = totalTokens
        headline.input = daily.reduce(0) { $0 + $1.input }
        headline.output = daily.reduce(0) { $0 + $1.output }
        headline.cacheRead = daily.reduce(0) { $0 + $1.cacheRead }
        headline.cacheWrite = daily.reduce(0) { $0 + $1.cacheWrite }
        headline.reasoning = headline.output / 3
        headline.activeDays = activeDays
        headline.currentStreak = 4
        headline.longestStreak = 11
        headline.peakHour = 15
        headline.favoriteModel = ModelRefLite(
            providerId: "anthropic", modelId: "claude-opus-5")
        headline.totalCost = totalCost
        headline.avgSessionTokens =
            headline.sessions == 0 ? 0 : totalTokens / Swift.max(1, headline.sessions)
        headline.avgTurnDurationMs = 26_400
        stats.headline = headline

        // The stats document identifies a model without its thinking level, so the mock
        // corpus must too — otherwise previews disagree with a real document.
        let refs = [
            ModelRefLite(providerId: "anthropic", modelId: "claude-opus-5"),
            ModelRefLite(providerId: "anthropic", modelId: "claude-sonnet-5"),
            ModelRefLite(providerId: "openai", modelId: "gpt-5"),
        ]
        let names = ["Opus 5", "Sonnet 5", "GPT-5"]
        let shares = [0.62, 0.27, 0.11]
        stats.models = zip(zip(refs, names), shares).map { pair, share in
            var stat = ModelStat(model: pair.0, displayName: pair.1)
            stat.share = share
            stat.turns = Int64(Double(totalTurns) * share)
            stat.totalTokens = Int64(Double(totalTokens) * share)
            stat.cost = totalCost * share
            stat.avgTtftMs = Int64(420 + rng.next(600))
            stat.avgOutputTps = 38 + rng.double(30)
            stat.errorRate = rng.double(0.04)
            return stat
        }
        stats.providers = [
            {
                var p = ProviderStat(providerId: "anthropic", displayName: "Anthropic")
                p.share = 0.89
                p.turns = Int64(Double(totalTurns) * 0.89)
                p.totalTokens = Int64(Double(totalTokens) * 0.89)
                p.cost = totalCost * 0.89
                return p
            }(),
            {
                var p = ProviderStat(providerId: "openai", displayName: "OpenAI")
                p.share = 0.11
                p.turns = Int64(Double(totalTurns) * 0.11)
                p.totalTokens = Int64(Double(totalTokens) * 0.11)
                p.cost = totalCost * 0.11
                return p
            }(),
        ]
        stats.tools = ["read", "edit", "bash", "grep", "write"].map { name in
            var tool = ToolStat(name: name)
            tool.invocations = Int64(20 + rng.next(300))
            tool.successRate = 0.86 + rng.double(0.13)
            tool.meanDurationMs = Int64(120 + rng.next(2_400))
            return tool
        }

        var ranks: [SessionRank] = []
        for (i, session) in MockCorpus.rankTitles.enumerated() {
            var rank = SessionRank(sessionId: "ses_rank_\(i)", title: session)
            rank.tokens = Int64(120_000 - i * 9_000)
            rank.durationMs = Int64(3_600_000 - i * 210_000)
            rank.turns = Int64(64 - i * 4)
            ranks.append(rank)
        }
        var boards = SessionLeaderboards()
        boards.byTokens = ranks
        boards.byDuration = ranks.sorted { $0.durationMs > $1.durationMs }
        boards.byTurns = ranks.sorted { $0.turns > $1.turns }
        stats.sessionsTop = boards

        var cache = CacheStats()
        cache.read = headline.cacheRead
        cache.write = headline.cacheWrite
        cache.hitRatio =
            headline.cacheRead + headline.cacheWrite == 0
            ? 0 : Double(headline.cacheRead) / Double(headline.cacheRead + headline.cacheWrite)
        cache.estimatedSavings = Double(headline.cacheRead) * 4.5e-6
        cache.daily = daily.map { bucket in
            var point = CachePoint(date: bucket.date)
            point.read = bucket.cacheRead
            point.write = bucket.cacheWrite
            return point
        }
        stats.cache = cache

        var cost = CostStats()
        cost.total = totalCost
        cost.byDay = daily.map { bucket in
            var point = CostPoint(date: bucket.date)
            point.cost = bucket.cost
            return point
        }
        cost.byProvider = [
            KeyedCost(key: "anthropic", cost: totalCost * 0.89),
            KeyedCost(key: "openai", cost: totalCost * 0.11),
        ]
        cost.byModel = zip(refs, shares).map { KeyedCost(key: $0, cost: totalCost * $1) }
        cost.projectedMonthly = activeDays < 3 ? 0 : totalCost / Double(Swift.max(1, days)) * 30
        stats.cost = cost

        stats.latency = refs.map { ref in
            var stat = LatencyStat(model: ref)
            stat.ttftP50 = Int64(480 + rng.next(300))
            stat.ttftP90 = stat.ttftP50 + Int64(400 + rng.next(600))
            stat.ttftP99 = stat.ttftP90 + Int64(900 + rng.next(1_500))
            stat.tpsP50 = 46 + rng.double(12)
            stat.tpsP90 = stat.tpsP50 + 14
            stat.tpsP99 = stat.tpsP90 + 9
            stat.samples = Int64(60 + rng.next(400))
            stat.histogram = (0..<8).map { bin in
                var bar = HistogramBin()
                bar.lowerMs = Int64(bin) * 250
                // The final bin is open-ended, exactly as the core emits it.
                bar.upperMs = bin == 7 ? nil : Int64(bin + 1) * 250
                bar.count = Int64(rng.next(90))
                return bar
            }
            return stat
        }

        return stats
    }

    static let rankTitles = [
        "Port the stats engine to chrono-tz",
        "Sidebar drag and drop",
        "Markdown streaming reflow",
        "Context ring animation",
        "Attachment thumbnails",
        "Command palette ranking",
        "Keychain round trip",
        "FFI free-while-streaming",
        "Heatmap quintiles",
        "Turn footer timing",
    ]
}

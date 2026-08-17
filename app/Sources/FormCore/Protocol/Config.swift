import Foundation

/// What `form_core_new` is handed (spec 00 §2).
public struct CoreConfig: Codable, Sendable, Equatable {
    public var dataDir: String
    /// Populate the store with demo sessions and usage history on first launch.
    public var seedMockData: Bool
    public var logLevel: String
    /// Multiplier on stub-harness timings. 1.0 is human-realistic; tests use 40–100.
    public var harnessSpeed: Double

    public init(
        dataDir: String,
        seedMockData: Bool = true,
        logLevel: String = "info",
        harnessSpeed: Double = 1.0
    ) {
        self.dataDir = dataDir
        self.seedMockData = seedMockData
        self.logLevel = logLevel
        self.harnessSpeed = harnessSpeed
    }

    /// `~/Library/Application Support/form`.
    public static func defaultDataDir() -> String {
        let base =
            FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("form").path
    }

    public static func standard() -> CoreConfig {
        CoreConfig(dataDir: defaultDataDir())
    }
}

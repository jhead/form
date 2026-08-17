import Foundation

/// The settings document, plus the API keys that deliberately do **not** live in it.
///
/// `updateSettings` replaces the whole document; the core normalizes and echoes it back as
/// `settings_changed`, and that echo is what this store renders — never the local edit
/// (spec 04 §2). Keys go to the Keychain and only `hasKey` crosses the boundary (F8.5).
@MainActor
@Observable
public final class SettingsStore {
    public private(set) var settings = Settings()
    public private(set) var isLoaded = false

    @ObservationIgnored private let client: CoreClient
    @ObservationIgnored private let keychain: KeychainStore

    public init(client: CoreClient, keychain: KeychainStore = KeychainStore()) {
        self.client = client
        self.keychain = keychain
    }

    /// Preview seeding — synchronous.
    func seed(_ settings: Settings) {
        self.settings = settings
        isLoaded = true
    }

    public func load() async {
        do {
            settings = try await client.query(GetSettings())
            isLoaded = true
        } catch {
            Log.stores.error(
                "getSettings failed: \(String(describing: error), privacy: .public)")
        }
    }

    public func apply(_ event: CoreEvent) {
        if case let .settingsChanged(settings) = event.kind {
            self.settings = settings
            isLoaded = true
        }
    }

    /// Edit and persist in one step. The local copy is updated optimistically so a toggle
    /// does not lag a round trip; the `settings_changed` echo replaces it.
    public func update(_ mutate: (inout Settings) -> Void) async throws {
        var next = settings
        mutate(&next)
        guard next != settings else { return }
        settings = next
        try await client.dispatch(.updateSettings(settings: next))
    }

    // MARK: - Appearance shortcuts, used by the shell

    public var themeMode: ThemeMode { settings.appearance.themeMode }

    public func setThemeMode(_ mode: ThemeMode) async throws {
        try await update { $0.appearance.themeMode = mode }
    }

    public func setSidebarCollapsed(_ collapsed: Bool) async throws {
        try await update { $0.appearance.sidebarCollapsed = collapsed }
    }

    public func setSidebarWidth(_ width: Double) async throws {
        try await update { $0.appearance.sidebarWidth = width }
    }

    public func setTextSizeMultiplier(_ value: Double) async throws {
        try await update { $0.appearance.textSizeMultiplier = value }
    }

    public func setDefaultModel(_ ref: ModelRef) async throws {
        try await update { $0.defaults.modelRef = ref }
    }

    // MARK: - API keys

    /// Whether the core believes a key exists. The value itself is never read for display.
    public func hasAPIKey(for providerId: String) -> Bool { settings.hasKey(for: providerId) }

    /// Writes the key to the Keychain, then records presence through `updateSettings`.
    /// Passing `nil` deletes it. The key never crosses the FFI boundary.
    public func setAPIKey(_ key: String?, for providerId: String) async throws {
        if let key, !key.isEmpty {
            try keychain.set(key, for: providerId)
        } else {
            try keychain.delete(providerId)
        }
        try await update { settings in
            var provider = settings.providers[providerId] ?? ProviderSettings()
            provider.hasKey = key?.isEmpty == false
            if provider.hasKey { provider.enabled = true }
            settings.providers[providerId] = provider
        }
    }

    /// Only for handing to a provider client. Never log or display the result.
    public func apiKey(for providerId: String) throws -> String? {
        try keychain.get(providerId)
    }

    /// Reconciles `hasKey` with what the Keychain actually holds — a restored machine can
    /// have the document without the keys.
    public func reconcileAPIKeys() async throws {
        var changed = false
        var next = settings
        for (id, provider) in next.providers {
            let present = ((try? keychain.get(id)) ?? nil) != nil
            if provider.hasKey != present {
                next.providers[id]?.hasKey = present
                changed = true
            }
        }
        guard changed else { return }
        settings = next
        try await client.dispatch(.updateSettings(settings: next))
    }

    public func setProviderEnabled(_ enabled: Bool, for providerId: String) async throws {
        try await update { settings in
            var provider = settings.providers[providerId] ?? ProviderSettings()
            provider.enabled = enabled
            settings.providers[providerId] = provider
        }
    }

    public func setBaseURLOverride(_ url: String?, for providerId: String) async throws {
        try await update { settings in
            var provider = settings.providers[providerId] ?? ProviderSettings()
            provider.baseUrlOverride = (url?.isEmpty == false) ? url : nil
            settings.providers[providerId] = provider
        }
    }

    // MARK: - Import / export (F9.3)

    public func exportJSON() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(settings)
    }

    public func importJSON(_ data: Data) async throws {
        let imported = try JSONDecoder().decode(Settings.self, from: data)
        settings = imported
        try await client.dispatch(.updateSettings(settings: imported))
    }
}

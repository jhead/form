import Foundation

/// The settings document (spec 04 §2). One document, versioned, replaced wholesale by
/// `updateSettings`; the core normalizes it and echoes the result back as
/// `settings_changed`, so the app renders what comes back rather than what it sent.
///
/// **API keys are never in this document and never cross the FFI boundary.** Swift owns
/// Keychain storage (`KeychainStore`); the core records only `hasKey` per provider (F8.5).
///
/// Every level carries an `unknown` bag. W4 is still adding sections, and a build that
/// dropped the keys it did not recognize would silently delete a setting made in a newer
/// build the next time the user changed anything (spec 04 §2: unknown fields survive a
/// round trip).
public struct Settings: Codable, Sendable, Equatable {
    public var version: Int
    public var general: GeneralSettings
    public var appearance: AppearanceSettings
    public var defaults: DefaultsSettings
    public var providers: [String: ProviderSettings]
    public var editor: EditorSettings?
    public var advanced: AdvancedSettings?
    /// Action id → key equivalent. Overrides only (F12.3).
    public var shortcuts: [String: String]?
    public var unknown: [String: JSONValue]

    public init(
        version: Int = 1,
        general: GeneralSettings = GeneralSettings(),
        appearance: AppearanceSettings = AppearanceSettings(),
        defaults: DefaultsSettings = DefaultsSettings(),
        providers: [String: ProviderSettings] = [:],
        editor: EditorSettings? = nil,
        advanced: AdvancedSettings? = nil,
        shortcuts: [String: String]? = nil,
        unknown: [String: JSONValue] = [:]
    ) {
        self.version = version
        self.general = general
        self.appearance = appearance
        self.defaults = defaults
        self.providers = providers
        self.editor = editor
        self.advanced = advanced
        self.shortcuts = shortcuts
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case version, general, appearance, defaults, providers, editor, advanced, shortcuts
    }

    private static let knownKeys: Set<String> = [
        "version", "general", "appearance", "defaults", "providers", "editor", "advanced",
        "shortcuts",
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decodeIfPresent(Int.self, forKey: .version) ?? 1
        general = try c.decodeIfPresent(GeneralSettings.self, forKey: .general) ?? .init()
        appearance = try c.decodeIfPresent(AppearanceSettings.self, forKey: .appearance) ?? .init()
        defaults = try c.decodeIfPresent(DefaultsSettings.self, forKey: .defaults) ?? .init()
        providers = try c.decodeIfPresent([String: ProviderSettings].self, forKey: .providers) ?? [:]
        editor = try c.decodeIfPresent(EditorSettings.self, forKey: .editor)
        advanced = try c.decodeIfPresent(AdvancedSettings.self, forKey: .advanced)
        shortcuts = try c.decodeIfPresent([String: String].self, forKey: .shortcuts)
        unknown = try decodeUnknownKeys(from: decoder, known: Self.knownKeys)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(version, forKey: .version)
        try c.encode(general, forKey: .general)
        try c.encode(appearance, forKey: .appearance)
        try c.encode(defaults, forKey: .defaults)
        try c.encode(providers, forKey: .providers)
        try c.encodeIfPresent(editor, forKey: .editor)
        try c.encodeIfPresent(advanced, forKey: .advanced)
        try c.encodeIfPresent(shortcuts, forKey: .shortcuts)
        try encodeUnknownKeys(unknown, to: encoder)
    }

    public func hasKey(for providerId: String) -> Bool {
        providers[providerId]?.hasKey ?? false
    }
}

public struct GeneralSettings: Codable, Sendable, Equatable {
    public var startupView: String
    public var confirmOnDelete: Bool
    public var autoTitleSessions: Bool
    public var unknown: [String: JSONValue]

    public init(
        startupView: String = "home", confirmOnDelete: Bool = true,
        autoTitleSessions: Bool = true, unknown: [String: JSONValue] = [:]
    ) {
        self.startupView = startupView
        self.confirmOnDelete = confirmOnDelete
        self.autoTitleSessions = autoTitleSessions
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case startupView, confirmOnDelete, autoTitleSessions
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        startupView = try c.decodeIfPresent(String.self, forKey: .startupView) ?? "home"
        confirmOnDelete = try c.decodeIfPresent(Bool.self, forKey: .confirmOnDelete) ?? true
        autoTitleSessions = try c.decodeIfPresent(Bool.self, forKey: .autoTitleSessions) ?? true
        unknown = try decodeUnknownKeys(
            from: decoder, known: ["startupView", "confirmOnDelete", "autoTitleSessions"])
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(startupView, forKey: .startupView)
        try c.encode(confirmOnDelete, forKey: .confirmOnDelete)
        try c.encode(autoTitleSessions, forKey: .autoTitleSessions)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct AppearanceSettings: Codable, Sendable, Equatable {
    public var themeMode: ThemeMode
    public var textSizeMultiplier: Double
    public var sidebarWidth: Double
    public var sidebarCollapsed: Bool
    public var showTurnFooters: Bool
    /// W4 adds this; typed here because the preferences surface needs a name for it.
    public var density: String?
    public var unknown: [String: JSONValue]

    public init(
        themeMode: ThemeMode = .system, textSizeMultiplier: Double = 1.0,
        sidebarWidth: Double = 300, sidebarCollapsed: Bool = false,
        showTurnFooters: Bool = true, density: String? = nil,
        unknown: [String: JSONValue] = [:]
    ) {
        self.themeMode = themeMode
        self.textSizeMultiplier = textSizeMultiplier
        self.sidebarWidth = sidebarWidth
        self.sidebarCollapsed = sidebarCollapsed
        self.showTurnFooters = showTurnFooters
        self.density = density
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case themeMode, textSizeMultiplier, sidebarWidth, sidebarCollapsed, showTurnFooters
        case density
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        themeMode = try c.decodeIfPresent(ThemeMode.self, forKey: .themeMode) ?? .system
        textSizeMultiplier = try c.decodeIfPresent(Double.self, forKey: .textSizeMultiplier) ?? 1
        sidebarWidth = try c.decodeIfPresent(Double.self, forKey: .sidebarWidth) ?? 300
        sidebarCollapsed = try c.decodeIfPresent(Bool.self, forKey: .sidebarCollapsed) ?? false
        showTurnFooters = try c.decodeIfPresent(Bool.self, forKey: .showTurnFooters) ?? true
        density = try c.decodeIfPresent(String.self, forKey: .density)
        unknown = try decodeUnknownKeys(
            from: decoder,
            known: [
                "themeMode", "textSizeMultiplier", "sidebarWidth", "sidebarCollapsed",
                "showTurnFooters", "density",
            ]
        )
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(themeMode, forKey: .themeMode)
        try c.encode(textSizeMultiplier, forKey: .textSizeMultiplier)
        try c.encode(sidebarWidth, forKey: .sidebarWidth)
        try c.encode(sidebarCollapsed, forKey: .sidebarCollapsed)
        try c.encode(showTurnFooters, forKey: .showTurnFooters)
        try c.encodeIfPresent(density, forKey: .density)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct DefaultsSettings: Codable, Sendable, Equatable {
    public var modelRef: ModelRef
    public var systemPrompt: String
    public var unknown: [String: JSONValue]

    public init(
        modelRef: ModelRef = ModelRef(
            providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high),
        systemPrompt: String = "",
        unknown: [String: JSONValue] = [:]
    ) {
        self.modelRef = modelRef
        self.systemPrompt = systemPrompt
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey { case modelRef, systemPrompt }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        modelRef =
            try c.decodeIfPresent(ModelRef.self, forKey: .modelRef)
            ?? ModelRef(providerId: "anthropic", modelId: "claude-opus-5", thinkingLevel: .high)
        systemPrompt = try c.decodeIfPresent(String.self, forKey: .systemPrompt) ?? ""
        unknown = try decodeUnknownKeys(from: decoder, known: ["modelRef", "systemPrompt"])
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(modelRef, forKey: .modelRef)
        try c.encode(systemPrompt, forKey: .systemPrompt)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

public struct ProviderSettings: Codable, Sendable, Equatable {
    public var enabled: Bool
    public var baseUrlOverride: String?
    /// Presence only. The key itself lives in the Keychain and never crosses the boundary.
    public var hasKey: Bool
    public var unknown: [String: JSONValue]

    public init(
        enabled: Bool = false, baseUrlOverride: String? = nil, hasKey: Bool = false,
        unknown: [String: JSONValue] = [:]
    ) {
        self.enabled = enabled
        self.baseUrlOverride = baseUrlOverride
        self.hasKey = hasKey
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey { case enabled, baseUrlOverride, hasKey }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        baseUrlOverride = try c.decodeIfPresent(String.self, forKey: .baseUrlOverride)
        hasKey = try c.decodeIfPresent(Bool.self, forKey: .hasKey) ?? false
        unknown = try decodeUnknownKeys(
            from: decoder, known: ["enabled", "baseUrlOverride", "hasKey"])
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(enabled, forKey: .enabled)
        try c.encodeIfPresent(baseUrlOverride, forKey: .baseUrlOverride)
        try c.encode(hasKey, forKey: .hasKey)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

/// Not in the core yet (W4). Optional so an absent section stays absent on the way back.
public struct EditorSettings: Codable, Sendable, Equatable {
    public var font: String?
    public var fontSize: Double?
    public var tabWidth: Int?
    public var wrapCode: Bool?
    public var showLineNumbers: Bool?
    public var unknown: [String: JSONValue]

    public init(
        font: String? = nil, fontSize: Double? = nil, tabWidth: Int? = nil,
        wrapCode: Bool? = nil, showLineNumbers: Bool? = nil, unknown: [String: JSONValue] = [:]
    ) {
        self.font = font
        self.fontSize = fontSize
        self.tabWidth = tabWidth
        self.wrapCode = wrapCode
        self.showLineNumbers = showLineNumbers
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey {
        case font, fontSize, tabWidth, wrapCode, showLineNumbers
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        font = try c.decodeIfPresent(String.self, forKey: .font)
        fontSize = try c.decodeIfPresent(Double.self, forKey: .fontSize)
        tabWidth = try c.decodeIfPresent(Int.self, forKey: .tabWidth)
        wrapCode = try c.decodeIfPresent(Bool.self, forKey: .wrapCode)
        showLineNumbers = try c.decodeIfPresent(Bool.self, forKey: .showLineNumbers)
        unknown = try decodeUnknownKeys(
            from: decoder,
            known: ["font", "fontSize", "tabWidth", "wrapCode", "showLineNumbers"]
        )
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(font, forKey: .font)
        try c.encodeIfPresent(fontSize, forKey: .fontSize)
        try c.encodeIfPresent(tabWidth, forKey: .tabWidth)
        try c.encodeIfPresent(wrapCode, forKey: .wrapCode)
        try c.encodeIfPresent(showLineNumbers, forKey: .showLineNumbers)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

/// Not in the core yet (W4). `dataDir` is display-only.
public struct AdvancedSettings: Codable, Sendable, Equatable {
    public var logLevel: String?
    public var harnessSpeed: Double?
    public var dataDir: String?
    public var unknown: [String: JSONValue]

    public init(
        logLevel: String? = nil, harnessSpeed: Double? = nil, dataDir: String? = nil,
        unknown: [String: JSONValue] = [:]
    ) {
        self.logLevel = logLevel
        self.harnessSpeed = harnessSpeed
        self.dataDir = dataDir
        self.unknown = unknown
    }

    private enum CodingKeys: String, CodingKey { case logLevel, harnessSpeed, dataDir }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        logLevel = try c.decodeIfPresent(String.self, forKey: .logLevel)
        harnessSpeed = try c.decodeIfPresent(Double.self, forKey: .harnessSpeed)
        dataDir = try c.decodeIfPresent(String.self, forKey: .dataDir)
        unknown = try decodeUnknownKeys(
            from: decoder, known: ["logLevel", "harnessSpeed", "dataDir"])
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(logLevel, forKey: .logLevel)
        try c.encodeIfPresent(harnessSpeed, forKey: .harnessSpeed)
        try c.encodeIfPresent(dataDir, forKey: .dataDir)
        try encodeUnknownKeys(unknown, to: encoder)
    }
}

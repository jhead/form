import FormCore
import FormDesign
import Foundation
import SwiftUI

public enum PreferencesTab: String, CaseIterable, Identifiable, Sendable {
    case general, providers, models, appearance, editor, shortcuts, advanced

    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .general: "General"
        case .providers: "Providers"
        case .models: "Models"
        case .appearance: "Appearance"
        case .editor: "Editor"
        case .shortcuts: "Shortcuts"
        case .advanced: "Advanced"
        }
    }

    public var systemImage: String {
        switch self {
        case .general: "gearshape"
        case .providers: "key"
        case .models: "cpu"
        case .appearance: "paintbrush"
        case .editor: "curlybraces"
        case .shortcuts: "command"
        case .advanced: "wrench.and.screwdriver"
        }
    }
}

/// The edit cycle behind the preferences sheet (spec 13 Part A).
///
/// ## Why there is a draft at all
///
/// `updateSettings` replaces the whole document and the core answers with a *normalized* one:
/// values are clamped, a bad model ref is repaired, missing sections are filled. The view has
/// to render that answer, not the keystroke that provoked it. But dispatching per keystroke
/// would round-trip a file write for every character of a base URL, so edits accumulate in
/// `draft` and flush 300 ms after the last one.
///
/// `settings` therefore reads `draft ?? store.settings`: the draft while the user is mid-edit,
/// and the core's document the instant the flush completes. The draft is only cleared when it
/// is still the thing that was sent — a keystroke landing during the round trip keeps its own
/// draft alive rather than being reverted by a stale echo.
@MainActor
@Observable
public final class PreferencesController {
    public let stores: CoreStores
    public let themeController: ThemeController

    public var tab: PreferencesTab
    public var modelSearch = ""

    /// What `SettingsTransfer` last reported, rendered inline rather than thrown away (F9.3).
    public var transferReport: SettingsTransferReport?
    /// A failed Keychain write or dispatch. Preferences is modal, so it says so in place.
    public var lastError: String?

    /// The debounce window from spec 13: long enough to coalesce typing, short enough that a
    /// toggle feels immediate.
    static let debounce = Duration.milliseconds(300)

    @ObservationIgnored private var draft: Settings?
    @ObservationIgnored private var flushTask: Task<Void, Never>?

    public init(
        stores: CoreStores,
        themeController: ThemeController,
        tab: PreferencesTab = .general
    ) {
        self.stores = stores
        self.themeController = themeController
        self.tab = tab
    }

    // MARK: - Reading

    public var settings: Settings { draft ?? stores.settings.settings }

    public var catalog: CatalogStore { stores.catalog }

    /// True while an edit has not yet been acknowledged — the sheet shows a quiet marker so a
    /// user closing the window immediately knows the write is still in flight.
    public var hasPendingEdit: Bool { draft != nil }

    // MARK: - Writing

    public func edit(_ mutate: (inout Settings) -> Void) {
        var next = settings
        mutate(&next)
        guard next != settings else { return }
        draft = next
        scheduleFlush()
    }

    /// A `Binding` over one field, routed through `edit` so every write is debounced the same
    /// way and every read comes from the current document.
    public func binding<Value>(_ keyPath: WritableKeyPath<Settings, Value>) -> Binding<Value> {
        Binding(
            get: { self.settings[keyPath: keyPath] },
            set: { value in self.edit { $0[keyPath: keyPath] = value } }
        )
    }

    private func scheduleFlush() {
        flushTask?.cancel()
        flushTask = Task { [weak self] in
            try? await Task.sleep(for: Self.debounce)
            guard !Task.isCancelled else { return }
            await self?.flush()
        }
    }

    /// Sends the draft now. Called by the debounce, and directly before anything that reads
    /// the document behind our back — an API-key write, an export, a reset.
    public func flush() async {
        flushTask?.cancel()
        flushTask = nil
        guard let pending = draft else { return }
        do {
            try await stores.settings.update { $0 = pending }
        } catch {
            lastError = "Could not save settings: \(error)"
            Log.ui.error("updateSettings failed: \(String(describing: error), privacy: .public)")
        }
        // A keystroke that landed during the round trip owns the draft now; leave it be.
        if draft == pending { draft = nil }
    }

    // MARK: - Live application (F9.2)

    /// Appearance changes have to repaint before the round trip completes, so they drive
    /// `ThemeController` directly *and* persist. The controller's setters rebuild the resolved
    /// theme; assigning its stored properties would not.
    public func setThemeMode(_ mode: ThemeMode) {
        themeController.setMode(mode)
        edit { $0.appearance.themeMode = mode }
    }

    public func setTextScale(_ scale: CGFloat) {
        themeController.setTextScale(scale)
        edit { $0.appearance.textSizeMultiplier = Double(themeController.textScale) }
    }

    /// Pulls the resolved theme back in line with the document — after an import, a reset, or
    /// a clamp the core applied to a value we sent.
    public func syncThemeFromSettings() {
        themeController.setMode(settings.appearance.themeMode)
        themeController.setTextScale(CGFloat(settings.appearance.textSizeMultiplier))
    }

    // MARK: - Models

    public func setDefaultModel(_ ref: ModelRef) {
        edit { settings in
            settings.defaults.modelRef = ref
            // Carry an effort the new model actually offers; the core would repair it, but
            // the picker would flicker through the invalid value first (F8.2).
            let levels = self.catalog.thinkingLevels(for: ref)
            if !levels.contains(settings.defaults.modelRef.thinkingLevel) {
                settings.defaults.modelRef.thinkingLevel = levels.contains(.high)
                    ? .high : (levels.first ?? .off)
            }
        }
    }

    public func setThinkingLevel(_ level: ThinkingLevel) {
        edit { $0.defaults.modelRef.thinkingLevel = level }
    }

    public var modelHits: [ModelHit] { catalog.search(modelSearch) }

    // MARK: - API keys (F8.5)

    /// The key is written to the Keychain and never to the document, never to the core, and
    /// never to a log. The caller drops its copy the moment this returns.
    public func setAPIKey(_ key: String, for providerId: String) async {
        await flush()
        do {
            try await stores.settings.setAPIKey(key, for: providerId)
            lastError = nil
        } catch {
            lastError = "Could not save the key for \(providerId): \(error)"
        }
    }

    public func clearAPIKey(for providerId: String) async {
        await flush()
        do {
            try await stores.settings.setAPIKey(nil, for: providerId)
            lastError = nil
        } catch {
            lastError = "Could not clear the key for \(providerId): \(error)"
        }
    }

    public func hasAPIKey(for providerId: String) -> Bool {
        settings.hasKey(for: providerId)
    }

    public func providerSettings(_ providerId: String) -> ProviderSettings {
        settings.providers[providerId] ?? ProviderSettings()
    }

    public func editProvider(_ providerId: String, _ mutate: (inout ProviderSettings) -> Void) {
        edit { settings in
            var provider = settings.providers[providerId] ?? ProviderSettings()
            mutate(&provider)
            settings.providers[providerId] = provider
        }
    }

    // MARK: - Import / export / reset (F9.3)

    public func exportSettings() async {
        await flush()
        do {
            transferReport = try SettingsTransfer.export(stores.settings.settings)
        } catch {
            transferReport = .failure("Export failed: \(error)")
        }
    }

    public func importSettings() async {
        await flush()
        guard let picked = SettingsTransfer.pickImportFile() else { return }
        switch SettingsTransfer.decode(picked) {
        case let .success(imported, notes):
            do {
                try await stores.settings.importJSON(SettingsTransfer.encode(imported))
                transferReport = .imported(url: picked.url, notes: notes)
                syncThemeFromSettings()
            } catch {
                transferReport = .failure("Could not apply the imported settings: \(error)")
            }
        case let .invalid(message):
            // The file is kept, not discarded — the user gets the reason and can fix it.
            transferReport = .failure(message)
        }
    }

    /// Everything back to the core's defaults. The Keychain is deliberately untouched: a
    /// factory reset of the *document* is not a request to lose credentials.
    public func resetToDefaults() async {
        flushTask?.cancel()
        draft = nil
        do {
            try await stores.settings.update { $0 = Settings() }
            syncThemeFromSettings()
            transferReport = .reset
        } catch {
            transferReport = .failure("Reset failed: \(error)")
        }
    }
}

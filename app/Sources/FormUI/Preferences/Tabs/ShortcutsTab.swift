import FormCore
import FormDesign
import SwiftUI

/// The shortcut table, grouped by category, each row recordable, conflicts flagged inline.
///
/// **This renders W14's table, it does not keep a copy of it.** `AppCommands.all` is the
/// single declaration of every command and its default key (spec 14 §1), and
/// `ShortcutResolver` is what turns that plus `settings.shortcuts` into the bindings the app
/// actually answers to. This tab only reads the resolver and writes the patch it hands back,
/// so a shortcut shown here and a shortcut that fires cannot disagree.
struct ShortcutsTab: View {
    @Environment(\.theme) private var theme
    let controller: PreferencesController

    /// A resolver of our own, fed from the document being edited. The app's live resolver
    /// belongs to `CommandCenter`; recomputing here means the tab reflects an override the
    /// instant it is typed, without waiting for the 300 ms flush to come back.
    @State private var resolver = ShortcutResolver()

    private var overrides: [String: String] { controller.settings.shortcutOverrides }

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.xxl) {
            ForEach(CommandCategory.allCases) { category in
                let commands = AppCommands.commands(in: category)
                if !commands.isEmpty {
                    PreferenceSection(title: category.title) {
                        ForEach(Array(commands.enumerated()), id: \.element.id) { index, command in
                            if index > 0 { FormDivider() }
                            row(command)
                        }
                    }
                }
            }
        }
        .preferencePane()
        .safeAreaInset(edge: .bottom, spacing: 0) { resetBar }
        .onAppear { resolver.apply(overrides: overrides) }
        .onChange(of: overrides) { _, next in resolver.apply(overrides: next) }
    }

    private func row(_ command: AppCommand) -> some View {
        let current = resolver.primaryKey(for: command.id)
        let displaced = resolver.displacedKeys(for: command.id)

        return PreferenceRow(
            title: command.title,
            help: conflictHelp(for: command, displaced: displaced),
            controlAlignment: .center
        ) {
            KeyRecorderField(
                current: current,
                defaultKey: command.defaultKey,
                conflict: conflictHelp(for: command, displaced: displaced),
                onRecord: { binding in record(binding, for: command) },
                onClear: { apply(resolver.settingsPatch(for: command.id, binding: nil)) }
            )
        }
    }

    /// The resolver drops a default that an override has taken, rather than letting both
    /// fire; saying so here is what turns a silent loss into a visible conflict.
    private func conflictHelp(for command: AppCommand, displaced: [KeyBinding]) -> String? {
        guard let lost = displaced.first else { return nil }
        guard let winner = resolver.command(bound: lost), winner.id != command.id else {
            return "\(lost.display) is taken."
        }
        return "\(lost.display) is used by “\(winner.title)”."
    }

    private func record(_ binding: KeyBinding, for command: AppCommand) {
        // Recording the built-in binding is a request to stop overriding, not to store a
        // duplicate of the default.
        let patch = resolver.settingsPatch(
            for: command.id, binding: binding == command.defaultKey ? nil : binding)
        apply(patch)
    }

    private func apply(_ patch: [String: String]) {
        controller.edit { $0.shortcutOverrides = patch }
        resolver.apply(overrides: patch)
    }

    private var resetBar: some View {
        HStack {
            Text(overrides.isEmpty ? "No overrides" : "\(overrides.count) override(s)")
                .typeStyle(theme.typography.micro)
                .foregroundStyle(theme.color.textTertiary)
            Spacer()
            FormButton("Reset to defaults", size: .small) {
                apply(resolver.clearedOverrides)
            }
            .disabled(overrides.isEmpty)
        }
        .padding(.horizontal, theme.metrics.spacing.xl)
        .padding(.vertical, theme.metrics.spacing.md)
        .background(theme.color.background)
        .overlay(alignment: .top) { FormDivider() }
    }
}

#Preview("Shortcuts") {
    PreferencesTabPreview(tab: .shortcuts)
}

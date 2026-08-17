import SwiftUI
import FormCore
import FormDesign

/// The chips above the field: scope, then W13's workspace-root chip and its picker
/// (spec 10 §6, F4.1–F4.5).
struct ComposerChipRow: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores

    /// Cached per core so the recent-roots list survives a rebuild — see
    /// `ComposerControllers`.
    private var workspace: WorkspaceRootController { ComposerControllers.workspace(for: stores) }

    var body: some View {
        HStack(spacing: theme.metrics.spacing.sm) {
            // `form` runs everything locally; the chip is the reference's scope affordance,
            // and there is nothing else to switch to.
            Chip("Local", systemImage: "laptopcomputer")

            WorkspaceFolderChip(controller: workspace)

            Spacer(minLength: 0)
        }
    }
}

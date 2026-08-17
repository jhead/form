import SwiftUI
import FormCore
import FormDesign

/// The chips above the field: scope, then W13's workspace-root chip and its picker
/// (spec 10 §6, F4.1–F4.5).
struct ComposerChipRow: View {
    @Environment(\.theme) private var theme

    let stores: CoreStores

    /// `@State`, resolved through `ComposerControllers`, for the same reason the intake is:
    /// the value SwiftUI throws away on each rebuild has to be the same object, or the
    /// recent-roots list restarts empty every time the composer re-renders.
    @State private var workspace: WorkspaceRootController

    init(stores: CoreStores) {
        self.stores = stores
        _workspace = State(initialValue: ComposerControllers.workspace(for: stores))
    }

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

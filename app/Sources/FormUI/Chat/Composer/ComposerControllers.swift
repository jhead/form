import Foundation
import FormCore

/// One `AttachmentIntake` and one `WorkspaceRootController` per core, created lazily.
///
/// Neither can be a `@State` initialised in the composer's `init`: SwiftUI evaluates that
/// argument every time the view value is rebuilt — which, for the composer, is every event of
/// every run — and only keeps the first. For `WorkspaceRootController` that would throw away
/// the recent-roots list on each rebuild; for `AttachmentIntake` it is worse, because its
/// initialiser claims `CoreStores.onEvent`, so each discarded copy would silently unhook the
/// live one and attachments would never leave the "adding" state.
///
/// A single-entry cache is enough: there is one `CoreStores` per process, and swapping it
/// (previews, tests) replaces the entry rather than leaking the old one.
@MainActor
enum ComposerControllers {
    private static var key: ObjectIdentifier?
    private static var intakeValue: AttachmentIntake?
    private static var workspaceValue: WorkspaceRootController?

    static func intake(for stores: CoreStores) -> AttachmentIntake {
        reset(if: stores)
        if let intakeValue { return intakeValue }
        let created = AttachmentIntake(stores: stores)
        intakeValue = created
        return created
    }

    static func workspace(for stores: CoreStores) -> WorkspaceRootController {
        reset(if: stores)
        if let workspaceValue { return workspaceValue }
        let created = WorkspaceRootController(stores: stores)
        workspaceValue = created
        return created
    }

    private static func reset(if stores: CoreStores) {
        let identity = ObjectIdentifier(stores)
        guard key != identity else { return }
        key = identity
        intakeValue = nil
        workspaceValue = nil
    }
}

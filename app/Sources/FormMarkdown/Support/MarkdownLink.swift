import AppKit
import FormCore
import Foundation

/// Opening a link from a transcript (F7.5).
///
/// The core has already downgraded anything that is not `http`, `https`, `mailto` or `file`
/// to plain text (spec 05 §3) — a `javascript:` URL never arrives as a link at all. The
/// allowlist is repeated here anyway: this is the function that hands a URL to the system,
/// and it should be safe on its own terms rather than because of what happened upstream.
enum MarkdownLink {
    static let allowedSchemes: Set<String> = ["http", "https", "mailto", "file"]

    /// `nil` for anything the renderer must not make clickable.
    static func url(from string: String) -> URL? {
        guard let url = URL(string: string),
            let scheme = url.scheme?.lowercased(),
            allowedSchemes.contains(scheme)
        else { return nil }
        return url
    }

    /// `file://` reveals in Finder rather than opening — a transcript link to a source file
    /// should show you where it is, not launch whatever is registered for `.rs`.
    @MainActor
    static func open(_ url: URL) {
        guard let scheme = url.scheme?.lowercased(), allowedSchemes.contains(scheme) else {
            Log.ui.error("refused to open a link with a disallowed scheme")
            return
        }
        if scheme == "file" {
            NSWorkspace.shared.activateFileViewerSelecting([url])
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    /// What the tooltip shows. `file://` is shown as a path, which is what the user recognises.
    static func display(_ url: URL) -> String {
        url.isFileURL ? url.path(percentEncoded: false) : url.absoluteString
    }
}

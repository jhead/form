import AppKit
import FormCore
import FormDesign
import SwiftUI

/// Remembers how tall an image rendered, so the second render of the same document does not
/// start from a placeholder and settle into a different height.
///
/// This is what makes F7.3's "no reflow" true for images: during streaming the transcript is
/// rebuilt many times a second, and without this every rebuild would restart the load and
/// re-collapse the block.
@MainActor
final class MarkdownImageSizes {
    static let shared = MarkdownImageSizes()
    private var heights: [String: CGFloat] = [:]

    func height(for key: String) -> CGFloat? { heights[key] }

    func record(_ height: CGFloat, for key: String) {
        guard height > 0, heights[key] != height else { return }
        heights[key] = height
    }
}

/// An inline image (spec 11 §2): `AsyncImage` for remote, a direct load for `file://` and
/// for attachment paths, capped height, rounded corners, and a placeholder that reserves
/// space.
struct MarkdownImageView: View {
    let url: String
    let alt: String
    let title: String?
    let metrics: MarkdownMetrics

    var body: some View {
        content
            .frame(maxWidth: .infinity, alignment: .leading)
            .formTooltip(title ?? (alt.isEmpty ? nil : alt))
            .accessibilityLabel(alt.isEmpty ? "image" : alt)
    }

    @ViewBuilder
    private var content: some View {
        if let local = localImage {
            image(Image(nsImage: local))
        } else if let remote = MarkdownLink.url(from: url) {
            AsyncImage(url: remote) { phase in
                switch phase {
                case let .success(loaded): image(loaded)
                case .failure: placeholder(failed: true)
                default: placeholder(failed: false)
                }
            }
        } else {
            placeholder(failed: true)
        }
    }

    private func image(_ image: Image) -> some View {
        image
            .resizable()
            .scaledToFit()
            .frame(maxHeight: metrics.imageMaxHeight)
            .clipShape(
                RoundedRectangle(cornerRadius: metrics.theme.metrics.radius.lg, style: .continuous)
            )
            .background(
                GeometryReader { proxy in
                    metrics.theme.color.surface.opacity(0)
                        .onAppear { MarkdownImageSizes.shared.record(proxy.size.height, for: url) }
                }
            )
    }

    private func placeholder(failed: Bool) -> some View {
        RoundedRectangle(cornerRadius: metrics.theme.metrics.radius.lg, style: .continuous)
            .fill(metrics.theme.color.surfaceRaised)
            .frame(height: reservedHeight)
            .overlay(
                Label(
                    alt.isEmpty ? url : alt,
                    systemImage: failed ? "exclamationmark.triangle" : "photo"
                )
                .typeStyle(metrics.theme.typography.caption)
                .foregroundStyle(metrics.theme.color.textTertiary)
                .padding(metrics.theme.metrics.spacing.lg)
            )
    }

    /// The height already measured for this URL, or the cap — never zero, which is the height
    /// that makes the transcript jump.
    private var reservedHeight: CGFloat {
        MarkdownImageSizes.shared.height(for: url) ?? metrics.imageMaxHeight
    }

    /// `file://`, and the bare absolute paths the attachment registry hands out.
    private var localImage: NSImage? {
        if url.hasPrefix("/") { return NSImage(contentsOfFile: url) }
        guard let parsed = URL(string: url), parsed.isFileURL else { return nil }
        return NSImage(contentsOf: parsed)
    }
}

#Preview("images reserve their space") {
    ThemePreview {
        MarkdownView(doc: MarkdownFixture.imagesOnly)
    }
    .frame(width: 900)
}

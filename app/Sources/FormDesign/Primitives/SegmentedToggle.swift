import SwiftUI

/// The sidebar's `Home` / `Code` control and the dashboard's `7d / 30d / All` selector.
/// Selected segment is a raised chip with a soft shadow; unselected is plain (spec 08 §1).
public struct SegmentedToggle<Value: Hashable>: View {
    public struct Segment: Identifiable {
        public let value: Value
        public let title: String
        public let systemImage: String?

        public var id: Value { value }

        public init(value: Value, title: String, systemImage: String? = nil) {
            self.value = value
            self.title = title
            self.systemImage = systemImage
        }
    }

    @Environment(\.theme) private var theme
    @Binding private var selection: Value
    private let segments: [Segment]
    private let height: CGFloat?

    @Namespace private var chip

    public init(selection: Binding<Value>, segments: [Segment], height: CGFloat? = nil) {
        _selection = selection
        self.segments = segments
        self.height = height
    }

    public var body: some View {
        HStack(spacing: 0) {
            ForEach(segments) { segment in
                segmentButton(segment)
            }
        }
        .padding(theme.metrics.spacing.xxs)
        .frame(height: (height ?? theme.metrics.segmentedHeight))
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                .fill(theme.color.surfaceRaised)
        )
        .animation(theme.motion.animation(.normal, curve: .emphasized), value: selection)
    }

    private func segmentButton(_ segment: Segment) -> some View {
        let isSelected = segment.value == selection
        return Button {
            selection = segment.value
        } label: {
            HStack(spacing: theme.metrics.spacing.sm) {
                if let systemImage = segment.systemImage {
                    Image(systemName: systemImage)
                        .font(.system(size: theme.metrics.iconSmall, weight: .medium))
                }
                Text(segment.title).typeStyle(theme.typography.uiMedium)
            }
            .foregroundStyle(isSelected ? theme.color.textPrimary : theme.color.textSecondary)
            .frame(maxWidth: .infinity)
            .frame(maxHeight: .infinity)
            .background {
                if isSelected {
                    RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                        .fill(theme.color.surface)
                        .shadow(color: theme.color.overlay.color.opacity(0.22), radius: 2, y: 1)
                        .matchedGeometryEffect(id: "selection", in: chip)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? [.isSelected, .isButton] : .isButton)
    }
}

#Preview("SegmentedToggle") {
    SegmentedTogglePreview()
}

private struct SegmentedTogglePreview: View {
    @State private var route = "home"
    @State private var period = "30d"

    var body: some View {
        ThemePreview {
            SegmentedToggle(
                selection: $route,
                segments: [
                    .init(value: "home", title: "Home", systemImage: "house"),
                    .init(value: "code", title: "Code", systemImage: "chevron.left.forwardslash.chevron.right"),
                ]
            )
            SegmentedToggle(
                selection: $period,
                segments: [
                    .init(value: "7d", title: "7d"),
                    .init(value: "30d", title: "30d"),
                    .init(value: "all", title: "All"),
                ],
                height: 26
            )
            .frame(width: 200)
        }
    }
}

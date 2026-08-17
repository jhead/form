import FormDesign
import SwiftUI

/// One labelled setting: title and optional explanation on the leading edge, the control on
/// the trailing edge. Every tab is built from these so the seven panes share one grid.
struct PreferenceRow<Control: View>: View {
    @Environment(\.theme) private var theme

    let title: String
    var help: String?
    var controlAlignment: VerticalAlignment = .firstTextBaseline
    @ViewBuilder let control: () -> Control

    var body: some View {
        HStack(alignment: controlAlignment, spacing: theme.metrics.spacing.xl) {
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xxs) {
                Text(title)
                    .typeStyle(theme.typography.ui)
                    .foregroundStyle(theme.color.textPrimary)
                if let help {
                    Text(help)
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textTertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            control()
                .frame(minWidth: PreferenceMetrics.controlColumn, alignment: .trailing)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

enum PreferenceMetrics {
    /// The trailing control column. Wide enough for a segmented control of three words and
    /// narrow enough to leave the explanation room to breathe at 720 pt.
    static let controlColumn: CGFloat = 220
    static let tabRailWidth: CGFloat = 168
}

/// A titled block of rows, separated by hairlines.
struct PreferenceSection<Content: View>: View {
    @Environment(\.theme) private var theme

    let title: String
    var footer: String?
    @ViewBuilder let content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
            SectionHeader(title)
            VStack(alignment: .leading, spacing: theme.metrics.spacing.lg) {
                content()
            }
            .padding(theme.metrics.spacing.xl)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                    .fill(theme.color.surface)
            )
            .overlay(
                RoundedRectangle(cornerRadius: theme.metrics.radius.lg, style: .continuous)
                    .strokeBorder(theme.color.border, lineWidth: theme.metrics.hairline * 2)
            )
            if let footer {
                Text(footer)
                    .typeStyle(theme.typography.micro)
                    .foregroundStyle(theme.color.textTertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// The switch every boolean setting uses, tinted to the theme rather than the system accent.
struct PreferenceToggle: View {
    @Environment(\.theme) private var theme
    @Binding var isOn: Bool
    var label: String = ""

    var body: some View {
        Toggle(label, isOn: $isOn)
            .labelsHidden()
            .toggleStyle(.switch)
            .tint(theme.color.accent.color)
            .frame(maxWidth: .infinity, alignment: .trailing)
    }
}

struct PreferenceOption<Value: Hashable>: Identifiable {
    let value: Value
    let label: String

    var id: Value { value }

    init(_ value: Value, _ label: String) {
        self.value = value
        self.label = label
    }
}

/// A menu picker over a labelled vocabulary. Used where a segmented control would not fit —
/// log levels, fonts, models.
struct PreferenceMenu<Value: Hashable>: View {
    @Environment(\.theme) private var theme

    @Binding var selection: Value
    let options: [PreferenceOption<Value>]

    var body: some View {
        Picker("", selection: $selection) {
            ForEach(options) { option in
                Text(option.label).tag(option.value)
            }
        }
        .labelsHidden()
        .pickerStyle(.menu)
        .typeStyle(theme.typography.ui)
        .tint(theme.color.accent.color)
        .frame(maxWidth: .infinity, alignment: .trailing)
    }
}

/// A slider with a live value readout. `ladder` snaps the thumb to discrete stops so a
/// keyboard step and a drag produce the same set of values.
struct PreferenceSlider: View {
    @Environment(\.theme) private var theme

    @Binding var value: Double
    let range: ClosedRange<Double>
    var ladder: [Double]?
    var format: (Double) -> String

    var body: some View {
        HStack(spacing: theme.metrics.spacing.lg) {
            Slider(value: bound, in: range)
                .tint(theme.color.accent.color)
            Text(format(value))
                .typeStyle(theme.typography.caption)
                .tabularFigures()
                .foregroundStyle(theme.color.textSecondary)
                .frame(width: PreferenceMetrics.controlColumn / 4, alignment: .trailing)
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
    }

    private var bound: Binding<Double> {
        Binding(
            get: { value },
            set: { raw in
                guard let ladder, !ladder.isEmpty else {
                    value = raw
                    return
                }
                value = ladder.min { abs($0 - raw) < abs($1 - raw) } ?? raw
            }
        )
    }
}

/// A one-line explanation of something that went wrong, in place rather than as a toast —
/// preferences is modal and a toast behind a sheet is unreadable.
struct PreferenceNotice: View {
    @Environment(\.theme) private var theme

    let message: String
    var tone: FormTone = .danger
    var details: [String] = []

    var body: some View {
        HStack(alignment: .top, spacing: theme.metrics.spacing.md) {
            Image(systemName: tone.systemImage)
                .typeStyle(theme.typography.caption)
                .foregroundStyle(tone.foreground(theme.color))
            VStack(alignment: .leading, spacing: theme.metrics.spacing.xs) {
                Text(message)
                    .typeStyle(theme.typography.caption)
                    .foregroundStyle(theme.color.textPrimary)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(details, id: \.self) { detail in
                    Text(detail)
                        .typeStyle(theme.typography.micro)
                        .foregroundStyle(theme.color.textTertiary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(theme.metrics.spacing.lg)
        .background(
            RoundedRectangle(cornerRadius: theme.metrics.radius.md, style: .continuous)
                .fill(tone.background(theme.color))
        )
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

extension View {
    /// The scrolling detail pane every tab sits in.
    func preferencePane() -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                self
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .modifier(PreferencePanePadding())
        }
    }
}

private struct PreferencePanePadding: ViewModifier {
    @Environment(\.theme) private var theme

    func body(content: Content) -> some View {
        content.padding(theme.metrics.spacing.xl)
    }
}

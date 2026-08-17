import Testing
@testable import FormDesign

@Test("themes are distinct")
func themesAreDistinct() {
    #expect(Theme.light != Theme.dark)
}

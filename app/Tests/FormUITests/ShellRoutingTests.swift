import FormCore
import Foundation
import Testing

@testable import FormUI

/// Spec 09 §5: the bounded route stack `⌘[` / `⌘]` traverse, and the flattened sidebar order
/// `⌘1`–`⌘9` index into. Both are pure logic behind the shell's layout, and both are easy to
/// break silently — a history that grows without bound or ranks that disagree with what the
/// rows show would look fine on screen.
@MainActor
struct ShellRoutingTests {

    // MARK: - RouteHistory

    @Test("navigating pushes, back and forward traverse")
    func backAndForward() {
        var history = RouteHistory(root: .home)
        #expect(!history.canGoBack)

        history.push(.session("a"))
        history.push(.session("b"))
        #expect(history.current == .session("b"))
        #expect(history.canGoBack)
        #expect(!history.canGoForward)

        #expect(history.back() == .session("a"))
        #expect(history.back() == .home)
        #expect(history.back() == nil)
        #expect(history.forward() == .session("a"))
        #expect(history.forward() == .session("b"))
    }

    @Test("a new destination truncates the forward branch")
    func newDestinationTruncates() {
        var history = RouteHistory(root: .home)
        history.push(.session("a"))
        history.push(.session("b"))
        history.back()

        history.push(.session("c"))
        #expect(!history.canGoForward)
        #expect(history.current == .session("c"))
        #expect(history.back() == .session("a"))
    }

    @Test("pushing the same route twice is a no-op")
    func duplicatePushIsIgnored() {
        var history = RouteHistory(root: .home)
        history.push(.session("a"))
        history.push(.session("a"))
        #expect(history.back() == .home)
    }

    @Test("the stack is bounded and drops the oldest entries")
    func boundedStack() {
        var history = RouteHistory(root: .home)
        for i in 0..<(RouteHistory.capacity * 2) { history.push(.session("s\(i)")) }
        #expect(history.entries.count == RouteHistory.capacity)
        #expect(history.current == .session("s\(RouteHistory.capacity * 2 - 1)"))
    }

    @Test("a deleted session leaves no hole to navigate into")
    func removingARoute() {
        var history = RouteHistory(root: .home)
        history.push(.session("a"))
        history.push(.session("b"))
        history.push(.session("a"))

        history.remove(.session("a"))
        #expect(!history.entries.contains(.session("a")))
        // home, b — and the two runs of `a` must not have collapsed into a repeated entry.
        #expect(history.entries == [.home, .session("b")])
        #expect(history.current == .session("b"))
    }

    // MARK: - AppState

    @Test("the Home/Code segment restores the last session")
    func segmentRestoresLastSession() {
        let state = AppState()
        #expect(state.segment == .home)

        state.segment = .code
        #expect(state.route == .noSession, "Code with no history has nothing to open")

        state.navigate(to: .session("a"))
        state.segment = .home
        #expect(state.route == .home)
        state.segment = .code
        #expect(state.route == .session("a"))
    }

    @Test("forgetting a session clears it from the route, the stack and Code")
    func forgetSession() {
        let state = AppState()
        state.navigate(to: .session("a"))
        state.navigate(to: .session("b"))

        state.forget(sessionId: "b")
        #expect(state.route == .session("a"))
        state.forget(sessionId: "a")
        #expect(state.route == .home)
        state.segment = .code
        #expect(state.route == .noSession)
    }

    @Test("replaceRoute corrects in place without adding history")
    func replaceRoute() {
        let state = AppState()
        state.navigate(to: .session("a"))
        state.replaceRoute(with: .home)
        #expect(state.route == .home)
        #expect(state.canGoBack)
        state.goBack()
        #expect(state.route == .home, "the replaced entry is the one that was there")
    }

    @Test("AppState satisfies the shape W14's command table depends on")
    func commandAppStateConformance() {
        let state: any CommandAppState = AppState()
        state.showSession("a")
        #expect(state.currentSessionId == "a")
        #expect(!state.isShowingHome)
        state.showHome()
        #expect(state.isShowingHome)
        #expect(state.currentSessionId == nil)
        #expect(state.canGoBack)
        state.goBack()
        #expect(state.currentSessionId == "a")
    }

    // MARK: - AppRoute persistence

    @Test("a route survives a launch as a string")
    func routeRoundTrips() {
        for route: AppRoute in [.home, .noSession, .session("ses_1")] {
            #expect(AppRoute(persisted: route.persisted) == route)
        }
        #expect(AppRoute(persisted: "session:") == nil)
        #expect(AppRoute(persisted: "nonsense") == nil)
    }
}

/// Spec 09 §2/§3: the sidebar's display order, its rank numbers, and the fact that a
/// collapsed group contributes neither.
@MainActor
struct SidebarOrderTests {

    private func store(collapsingFirstGroup: Bool = false) -> SessionStore {
        let groups = [
            SessionGroup(id: "g1", name: "Work", index: 0, collapsed: collapsingFirstGroup),
            SessionGroup(id: "g2", name: "Open source", index: 1),
        ]
        let model = ModelRef(providerId: "anthropic", modelId: "claude-opus-5")
        let sessions = [
            SessionSummary(id: "b", title: "b", groupId: "g1", index: 1, modelRef: model),
            SessionSummary(id: "a", title: "a", groupId: "g1", index: 0, modelRef: model),
            SessionSummary(id: "c", title: "c", groupId: "g2", index: 0, modelRef: model),
            SessionSummary(id: "u", title: "u", groupId: nil, index: 0, modelRef: model),
            SessionSummary(
                id: "gone", title: "gone", groupId: nil, index: 1, modelRef: model,
                archived: true),
        ]
        return SessionStore(
            groups: groups, sessions: sessions, client: CoreClient(mock: MockTransport()))
    }

    @Test("sections are groups in order, then Ungrouped, and archived rows are hidden")
    func sectionOrder() {
        let sections = SidebarOrder.sections(in: store())
        #expect(sections.map(\.name) == ["Work", "Open source", "Ungrouped"])
        #expect(sections.map { $0.sessions.map(\.id) } == [["a", "b"], ["c"], ["u"]])
    }

    @Test("rows are ordered by the core's dense index, so a drag sticks")
    func indexOrdering() {
        let sessions = SidebarOrder.sessions(
            in: SessionGroup(id: "g1", name: "Work", index: 0), store: store())
        #expect(sessions.map(\.id) == ["a", "b"], "index 0 before index 1, not by updatedAt")
    }

    @Test("ranks follow the visible order and skip collapsed groups")
    func ranksSkipCollapsedGroups() {
        #expect(SidebarOrder.ranks(in: store()) == ["a": 1, "b": 2, "c": 3, "u": 4])

        let collapsed = store(collapsingFirstGroup: true)
        #expect(SidebarOrder.ranks(in: collapsed) == ["c": 1, "u": 2])
        #expect(SidebarOrder.session(rank: 1, in: collapsed)?.id == "c")
        #expect(SidebarOrder.session(rank: 3, in: collapsed) == nil)
    }

    @Test("a pinned session sorts to the top of its group")
    func pinnedFirst() {
        let model = ModelRef(providerId: "anthropic", modelId: "claude-opus-5")
        let store = SessionStore(
            groups: [],
            sessions: [
                SessionSummary(id: "a", title: "a", index: 0, modelRef: model),
                SessionSummary(id: "p", title: "p", index: 5, modelRef: model, pinned: true),
            ],
            client: CoreClient(mock: MockTransport())
        )
        #expect(SidebarOrder.visibleSessions(in: store).map(\.id) == ["p", "a"])
    }
}

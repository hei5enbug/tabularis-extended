import Cocoa
import WebKit
import CoreFoundation

final class Harness: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
    private let application = NSApplication.shared
    private var window: NSWindow?
    private var webView: WKWebView?
    private var finished = false
    private let url: URL
    private let root: URL
    private let checks = ["open_ack", "real_worker", "worker_geojson", "real_webgl", "canvas_size", "feature_options", "null_empty", "css_loaded", "raw_detail", "safe_text", "stale_rejected", "updated_ack", "close_ack", "worker_terminated", "assets_disposed", "css_removed", "modal_unmounted", "root_shutdown", "unsubscribed", "no_external_requests", "no_static_imports", "query_snapshot_only"]

    init(url: URL, root: URL) {
        self.url = url
        self.root = root
        super.init()
    }

    func start() {
        application.setActivationPolicy(.regular)
        let controller = WKUserContentController()
        controller.add(self, name: "v1Result")
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.userContentController = controller
        let view = WKWebView(frame: NSRect(x: 0, y: 0, width: 1280, height: 900), configuration: configuration)
        view.navigationDelegate = self
        webView = view
        let ownWindow = NSWindow(contentRect: view.frame, styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        ownWindow.title = "Tabularis V1 WKWebView fixture"
        ownWindow.contentView = view
        ownWindow.center()
        ownWindow.makeKeyAndOrderFront(nil)
        window = ownWindow
        application.activate(ignoringOtherApps: true)
        DispatchQueue.main.asyncAfter(deadline: .now() + 60) { [weak self] in self?.fail("NATIVE_TIMEOUT") }
        let escapedOrigin = NSRegularExpression.escapedPattern(for: "http://127.0.0.1:\(url.port!)/")
        let rules: [[String: Any]] = [
            ["trigger": ["url-filter": "^https?://"], "action": ["type": "block"]],
            ["trigger": ["url-filter": "^" + escapedOrigin], "action": ["type": "ignore-previous-rules"]]
        ]
        do {
            let rulesDirectory = root.appendingPathComponent("content-rules", isDirectory: true)
            try FileManager.default.createDirectory(at: rulesDirectory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            let encoded = try JSONSerialization.data(withJSONObject: rules)
            guard let text = String(data: encoded, encoding: .utf8) else { fail("RULES_FAILED"); return }
            WKContentRuleListStore(url: rulesDirectory).compileContentRuleList(forIdentifier: "v1-loopback-only", encodedContentRuleList: text) { [weak self, weak view] list, error in
                guard let self = self, !self.finished else { return }
                guard error == nil, let list = list, let view = view else { self.fail("RULES_FAILED"); return }
                controller.add(list)
                view.load(URLRequest(url: self.url))
            }
        } catch { fail("RULES_FAILED") }
    }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard !finished, message.name == "v1Result", message.frameInfo.isMainFrame,
              let origin = message.frameInfo.request.url, origin.scheme == "http", origin.host == "127.0.0.1", origin.port == url.port,
              let body = message.body as? [String: Any], JSONSerialization.isValidJSONObject(body),
              let raw = try? JSONSerialization.data(withJSONObject: body), raw.count <= 65536,
              let pass = body["pass"] as? NSNumber, CFGetTypeID(pass) == CFBooleanGetTypeID(),
              body["evidence"] as? String == "actual_macos_wkwebview_with_mock_service" else { fail("INVALID_REPORT"); return }
        if pass.boolValue {
            guard let values = body["checks"] as? [String: Any], Set(values.keys) == Set(checks), checks.allSatisfy({ name in
                guard let value = values[name] as? NSNumber else { return false }
                return CFGetTypeID(value) == CFBooleanGetTypeID() && value.boolValue
            }) else { fail("INVALID_REPORT"); return }
        }
        var report = body
        report["native"] = ["os": ProcessInfo.processInfo.operatingSystemVersionString, "webkit": Bundle(for: WKWebView.self).object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "unknown", "non_persistent_store": true]
        guard let encoded = try? JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]), encoded.count <= 65536 else { fail("INVALID_REPORT"); return }
        finish(encoded, status: pass.boolValue ? 0 : 1)
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let target = navigationAction.request.url, target == url else { decisionHandler(.cancel); fail("NAVIGATION_DENIED"); return }
        decisionHandler(.allow)
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { fail("NAVIGATION_FAILED") }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) { fail("NAVIGATION_FAILED") }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { fail("WEB_CONTENT_TERMINATED") }

    private func fail(_ code: String) {
        guard let data = try? JSONSerialization.data(withJSONObject: ["pass": false, "code": code, "evidence": "actual_macos_wkwebview_with_mock_service"], options: [.sortedKeys]) else { return }
        finish(data, status: 1)
    }
    private func finish(_ report: Data, status: Int32) {
        guard !finished else { return }
        finished = true
        webView?.stopLoading()
        webView?.configuration.userContentController.removeScriptMessageHandler(forName: "v1Result")
        webView?.navigationDelegate = nil
        window?.contentView = nil
        window?.close()
        window = nil
        webView = nil
        FileHandle.standardOutput.write(report)
        FileHandle.standardOutput.write(Data([10]))
        DispatchQueue.main.async { exit(status) }
    }
}

guard CommandLine.arguments.count == 3,
      let navigation = URL(string: CommandLine.arguments[1]), navigation.scheme == "http", navigation.host == "127.0.0.1",
      let port = navigation.port, port > 0 && port <= 65535, navigation.path == "/index.html",
      navigation.user == nil, navigation.password == nil, navigation.query == nil, navigation.fragment == nil,
      CommandLine.arguments[2].hasPrefix("/tmp/tabularis-v1-") else {
    FileHandle.standardError.write(Data("INVALID_HARNESS_ARGUMENT\n".utf8))
    exit(1)
}
let harness = Harness(url: navigation, root: URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true))
harness.start()
NSApplication.shared.run()

import AppKit

// Renders each proposed Bubble alert as a real NSAlert and captures its window.
let out = CommandLine.arguments[1]
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
app.applicationIconImage = NSImage(contentsOfFile: out + "/icon.png")

struct Spec {
    let id: String
    let title: String
    let info: String
    let buttons: [String]
    var style: NSAlert.Style = .warning
    var destructive = false
    var checks: [(String, Bool)] = []
    var suppression: String? = nil
}

let specs: [Spec] = [
    Spec(id: "problem", title: "Bubble couldn’t open “Morning Show”.",
         info: "The session file is damaged or was made by a newer Bubble.\n\nsession.json: expected value at line 1 column 1",
         buttons: ["OK", "Copy Details"]),
    Spec(id: "save", title: "Do you want to save the changes you made to “Test Package”?",
         info: "Your changes will be lost if you don’t save them.",
         buttons: ["Save", "Cancel", "Don’t Save"]),
    Spec(id: "recover", title: "Restore unsaved changes to “Test Package”?",
         info: "Bubble closed before they were saved. Restoring doesn’t change the session file until you save.",
         buttons: ["Restore", "Discard Changes"], destructive: true),
    Spec(id: "import", title: "Import “Studio 1.dta”?",
         info: "Add its carts to the open session, or start a new session from it.",
         buttons: ["Add to This Session", "New Session…", "Cancel"]),
    Spec(id: "convert", title: "Convert 3 audio files to WAV?",
         info: "They aren’t WAV at 48 kHz yet. The originals stay in the session’s Audio folder.",
         buttons: ["Convert", "Not Now"]),
    Spec(id: "delete-page", title: "Delete the page “Morning Show”?",
         info: "Its carts are deleted with it. You can undo this.",
         buttons: ["Delete", "Cancel"], destructive: true),
    Spec(id: "missing", title: "3 audio files are missing.",
         info: "“intro.wav”, “sting 2.wav” and 1 more. Locate one and Bubble also finds others that moved with it.",
         buttons: ["Locate…", "Later"]),
    Spec(id: "paste-attrs", title: "Paste attributes from “Morning News”?",
         info: "Choose what to copy to the selected cart.",
         buttons: ["Paste", "Cancel"], style: .informational,
         checks: [("Gain and mute", true), ("Pan", true), ("Fades", true), ("Colour", false),
                  ("Loop", false), ("Behaviour (retrigger, stop others, group)", false)]),
]

func capture(_ s: Spec, _ look: String) {
    let a = NSAlert()
    a.icon = NSImage(contentsOfFile: out + "/icon.png")
    a.alertStyle = s.style
    a.messageText = s.title
    a.informativeText = s.info
    for b in s.buttons { a.addButton(withTitle: b) }
    if s.destructive, #available(macOS 11.0, *) {
        a.buttons.first?.hasDestructiveAction = true
    }
    if !s.checks.isEmpty {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        for (t, on) in s.checks {
            let c = NSButton(checkboxWithTitle: t, target: nil, action: nil)
            c.state = on ? .on : .off
            stack.addArrangedSubview(c)
        }
        stack.frame = NSRect(x: 0, y: 0, width: 300, height: stack.fittingSize.height)
        a.accessoryView = stack
    }
    a.layout()
    let w = a.window
    w.appearance = NSAppearance(named: look == "dark" ? .darkAqua : .aqua)
    w.center()
    w.orderFrontRegardless()
    w.makeKey()
    let until = Date().addingTimeInterval(0.6)
    while Date() < until { RunLoop.current.run(until: Date().addingTimeInterval(0.05)) }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    p.arguments = ["-x", "-o", "-l", "\(w.windowNumber)", "\(out)/\(s.id)-\(look).png"]
    try? p.run()
    p.waitUntilExit()
    w.orderOut(nil)
}

for look in ["light", "dark"] {
    for s in specs { capture(s, look) }
}
print("done")

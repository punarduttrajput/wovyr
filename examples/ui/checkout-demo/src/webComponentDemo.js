// Deliberately plain JS, no JSX/build-time React authored here — this is the
// smoke test that `<apex-ui-frame>` (RDR-402) is usable by a host that has
// never touched React, proving the web-component wrapper actually decouples
// @apex/ui-react from a React-authored host page.
import "@apex/ui-react/web-component";
import "@apex/ui-react/styles.css";

const sampleFrame = {
  schema_version: "1.0.0",
  title: "Confirm shipment",
  root: {
    type: "column",
    children: [
      { type: "text", text: "Ship 3 boxes to the address on file?" },
      { type: "button", action: "approve", label: "Approve", class: "approve" },
      { type: "button", action: "cancel", label: "Cancel", class: "cancel" },
    ],
  },
};

const log = document.getElementById("log");
const el = document.getElementById("frame");

el.frame = sampleFrame;

el.addEventListener("decide", (event) => {
  const { decision } = event.detail;
  log.textContent = `decide event received: action=${decision.action}`;
  // Resolve immediately — a real host would attach the actual API call here:
  // event.detail.result = apexClient.ui.decide(frameId, decision);
});

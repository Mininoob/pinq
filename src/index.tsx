/* @refresh reload */
import { render } from "solid-js/web";
import "./styles.css";
import { initTheme } from "./theme";
import Pad from "./pad/Pad";
import Note from "./note/Note";
import Preview from "./note/Preview";
import Toast from "./note/Toast";
import Settings from "./panels/Settings";
import Library from "./panels/Library";
import Find from "./panels/Find";
import Viewer from "./panels/Viewer";

// Every Pinq window loads this same app; the window's role travels in the URL.
const params = new URLSearchParams(window.location.search);
const role = params.get("role") ?? "pad";

void initTheme();

function App() {
  switch (role) {
    case "note":
      return <Note id={params.get("id") ?? ""} />;
    case "preview":
      return <Preview />;
    case "toast":
      return <Toast />;
    case "settings":
      return <Settings />;
    case "library":
      return <Library />;
    case "find":
      return <Find />;
    case "viewer":
      return <Viewer noteId={params.get("note") ?? ""} hash={params.get("hash") ?? ""} />;
    default: {
      const edge = params.get("edge");
      return (
        <Pad
          edge={edge === "left" || edge === "top" || edge === "bottom" ? edge : "right"}
        />
      );
    }
  }
}

render(App, document.getElementById("root")!);

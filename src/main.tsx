import { createRoot } from "react-dom/client";
import { Capsule } from "./components/Capsule";
import { ChatWindow } from "./components/ChatWindow";
import { EdgeGlow } from "./components/EdgeGlow";
import { SettingsWindow } from "./components/SettingsWindow";
import "./styles.css";

const view = new URLSearchParams(window.location.search).get("view") ?? "main";

const app = view === "edge"
  ? <EdgeGlow />
  : view === "chat"
    ? <ChatWindow />
    : view === "settings"
      ? <SettingsWindow />
      : <Capsule />;

createRoot(document.getElementById("root")!).render(app);

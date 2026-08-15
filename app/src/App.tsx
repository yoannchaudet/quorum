import { useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import Settings from "./Settings";
import "./App.css";

function App() {
  const [selectedProject, setSelectedProject] = useState<string | null>(null);

  const projects = [
    { id: "1", name: "Quorum", path: "~/projects/quorum" },
    { id: "2", name: "Tauri App", path: "~/projects/tauri-app" },
  ];

  return (
    <div className="flex h-screen w-full bg-white overflow-hidden">
      {/* Sidebar */}
      <div
        className="w-64 bg-slate-50 border-r border-slate-200 flex flex-col pt-8"
        onPointerDown={(e) => {
          console.log("Sidebar pointer down", e.target, e.currentTarget);
          if (e.target === e.currentTarget) {
            console.log("Starting drag...");
            getCurrentWindow().startDragging().catch(console.error);
          }
        }}
      >
        <div className="px-4 pb-2 text-xs font-semibold text-slate-500 uppercase tracking-wider pointer-events-none">
          Projects
        </div>
        <div className="flex-1 overflow-y-auto">
          {projects.map((project) => (
            <div
              key={project.id}
              onClick={() => setSelectedProject(project.id)}
              className={`px-4 py-2 mx-2 rounded-md cursor-default text-sm ${
                selectedProject === project.id
                  ? "bg-blue-500 text-white"
                  : "text-slate-700 hover:bg-slate-200"
              }`}
            >
              {project.name}
            </div>
          ))}
        </div>
        <div className="px-4 pb-2 text-xs font-semibold text-slate-500 uppercase tracking-wider pointer-events-none mt-6">
          System
        </div>
        <div className="">
          <div
            onClick={() => setSelectedProject("settings")}
            className={`px-4 py-2 mx-2 rounded-md cursor-default text-sm ${
              selectedProject === "settings"
                ? "bg-blue-500 text-white"
                : "text-slate-700 hover:bg-slate-200"
            }`}
          >
            Settings
          </div>
        </div>
        <div className="p-4 border-t border-slate-200">
          <button className="w-full text-left text-sm text-slate-500 hover:text-slate-800 cursor-default">
            + Add Project
          </button>
        </div>
      </div>

      {/* Main Content */}
      <div className="flex-1 flex flex-col bg-white">
        {/* Header */}
        <header
          className="h-14 border-b border-slate-200 flex items-center px-6"
          onPointerDown={(e) => {
            console.log("Header pointer down", e.target, e.currentTarget);
            if (e.target === e.currentTarget) {
              console.log("Starting drag...");
              getCurrentWindow().startDragging().catch(console.error);
            }
          }}
        >
          <h1 className="text-lg font-medium text-slate-800 pointer-events-none">
          {selectedProject === "settings"
              ? "Settings"
              : selectedProject
              ? projects.find((p) => p.id === selectedProject)?.name
              : "Select a project"}
          </h1>
        </header>

        {/* Content Area */}
        <main className="flex-1 p-6 overflow-y-auto">
          {selectedProject === "settings" ? (
            <Settings />
          ) : selectedProject ? (
            <div className="max-w-3xl mx-auto">
              <h2 className="text-2xl font-semibold mb-4">Plan Work</h2>
              <div className="bg-slate-50 border border-slate-200 rounded-lg p-6">
                <p className="text-slate-600 mb-4">
                  Define what you want to build in{" "}
                  <strong>
                    {projects.find((p) => p.id === selectedProject)?.name}
                  </strong>
                  .
                </p>
                <textarea
                  className="w-full h-32 p-3 border border-slate-300 rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent resize-none text-sm"
                  placeholder="Describe your next feature or fix..."
                />
                <div className="mt-4 flex justify-end">
                  <button className="px-4 py-2 bg-blue-500 text-white rounded-md hover:bg-blue-600 text-sm font-medium transition-colors">
                    Create Plan
                  </button>
                </div>
              </div>

              <div className="mt-8">
                <h3 className="text-lg font-medium mb-3">Current Plan</h3>
                <div className="space-y-3">
                  {/* Placeholder plan items */}
                  <div className="flex items-center p-3 border border-slate-200 rounded-md">
                    <input
                      type="checkbox"
                      className="mr-3 h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
                    />
                    <span className="text-sm">
                      Implement initial Tauri sidebar UI
                    </span>
                  </div>
                  <div className="flex items-center p-3 border border-slate-200 rounded-md">
                    <input
                      type="checkbox"
                      className="mr-3 h-4 w-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
                    />
                    <span className="text-sm">
                      Connect Rust backend for project detection
                    </span>
                  </div>
                </div>
              </div>
            </div>
          ) : (
            <div className="h-full flex items-center justify-center text-slate-400">
              <p>Select a project from the sidebar to start planning</p>
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

export default App;

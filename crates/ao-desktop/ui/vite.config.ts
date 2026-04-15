import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [tailwindcss(), react()],
  server: {
    port: 1420,
    strictPort: true
  },
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts"
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("node_modules/react-dom/")) {
            return "react-dom";
          }
          if (id.includes("node_modules/react/")) {
            return "react";
          }
          if (id.includes("node_modules/@xterm/")) {
            return "xterm";
          }
        }
      }
    }
  }
});


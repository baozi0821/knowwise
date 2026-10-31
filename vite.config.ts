import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// M00:前端构建基线。M13 拥有最终组装;此处只提供工具链配置。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "chrome110",
    outDir: "dist",
  },
});

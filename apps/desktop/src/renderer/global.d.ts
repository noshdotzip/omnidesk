import type { UltideskApi } from "../preload/preload.js";

declare global {
  interface Window {
    ultidesk: UltideskApi;
  }
}

export {};

// Where the API lives.
//
//   "" (empty)      -> same origin. Use this when the Rust server serves this
//                      folder itself (single binary, one origin, HTTPS on top).
//   "https://..."   -> point a GitHub Pages copy at a server hosted elsewhere.
window.RING_CONFIG = {
  apiBase: "",
  // Shown in the UI so people know which instance they're on.
  instance: "referral-ring"
};

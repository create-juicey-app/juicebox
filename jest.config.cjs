module.exports = {
  testEnvironment: "jsdom",
  testMatch: ["<rootDir>/frontend_tests/**/*.test.js"],
  transform: {
    "^.+\\.[tj]sx?$": ["babel-jest", { presets: ["@babel/preset-env"] }],
  },
  moduleFileExtensions: ["js", "jsx", "json", "mjs"],
  roots: ["<rootDir>"],
  collectCoverage: true,
  collectCoverageFrom: [
    "public/js/**/*.js",
    "!public/js/owned.js",
    "!public/js/upload.js",
    "!public/js/thumbgen.js",
    "!public/js/events.js",
    "!public/js/file-hash-worker.js",
    "!public/js/other.js",
  ],
  coverageReporters: ["text", "lcov"],
  coveragePathIgnorePatterns: ["/node_modules/"],
};

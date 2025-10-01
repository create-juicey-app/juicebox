module.exports = {
  testEnvironment: "jsdom",
  testMatch: ["<rootDir>/frontend_tests/**/*.test.js"],
  transform: {
    "^.+\\.[tj]sx?$": ["babel-jest", { presets: ["@babel/preset-env"] }]
  },
  moduleFileExtensions: ["js", "jsx", "json", "mjs"],
  roots: ["<rootDir>"]
};

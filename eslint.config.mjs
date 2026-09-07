import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["**/filesystem/**", "**/node_modules/**"] },
  ...tseslint.configs.recommended,
  {
    files: ["**/*.ts"],
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
);

// ESLint for the extension client, with typescript-eslint's strict rules as
// CONTRIBUTING.md asks for TypeScript.

import tseslint from 'typescript-eslint'

export default tseslint.config(...tseslint.configs.strict, {
  ignores: ['out/', 'node_modules/'],
})

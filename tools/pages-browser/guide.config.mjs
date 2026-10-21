import { defineConfig } from '@playwright/test';
import landing from './playwright.config.mjs';

// A separate bounded suite keeps the landing checks and
// their absolute deadlines unchanged. Both suites are publication gates.
export default defineConfig({
  ...landing,
  testMatch: 'guide.spec.mjs',
  metadata: { ...landing.metadata, suite: 'guide', browserCases: 12 },
});

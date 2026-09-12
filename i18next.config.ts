import { defineConfig } from 'i18next-cli';

export default defineConfig({
  locales: ['en', 'de', 'pl', 'zh-CN', 'zh-TW', 'es', 'fr', 'it', 'pt', 'ja', 'ko', 'ru', 'ca'],
  extract: {
    input: ['src/**/*.{ts,tsx}'],
    // Publish strings are registered at runtime via i18n.addResourceBundle so the
    // feature adds no keys to the thirteen locale files. See publish.i18n.ts.
    ignore: ['src/components/panel/right/publish/**'],
    output: 'src/i18n/locales/{{language}}.json',
    defaultNS: false,
    removeUnusedKeys: false,
    sort: true,
    defaultValue: '',
  },
});

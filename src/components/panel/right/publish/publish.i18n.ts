import i18n from 'i18next';

/**
 * Publish strings live here rather than in src/i18n/locales/*.json so that the
 * feature adds no conflict sites to the thirteen upstream locale files.
 *
 * Two things are needed to keep this out of tree:
 *  - `extract.ignore` in i18next.config.ts, so the CLI does not demand these
 *    keys in every locale file.
 *  - the `PublishTranslations` intersection in src/@types/i18next.d.ts, so
 *    `t('publish.*')` type-checks against the CustomTypeOptions resources.
 */
export const publishResources = {
  panel: {
    title: 'Publish',
    loading: 'Checking your SmugMug connection…',
    retry: 'Try again',
  },
  smugmug: {
    // Named by the backend's AuthChallenge.instructions_key.
    authInstructions:
      'Sign in to SmugMug in your browser and approve access. SmugMug then shows a six-digit code: paste it below.',
    setup: {
      intro:
        'RapidRAW publishes to SmugMug with an API key that belongs to you, not to RapidRAW. Getting one takes a couple of minutes and is free with any SmugMug account.',
      applyLink: 'Apply for a SmugMug API key',
      applyHint: 'Any application name will do. Once approved, SmugMug shows you a key and a secret.',
      keyLabel: 'API key',
      secretLabel: 'API secret',
      privacy:
        'Your key and secret are kept in your system keychain. They never leave this computer, except to sign requests sent to SmugMug.',
      save: 'Save key',
      saving: 'Saving…',
    },
    authorise: {
      stepOne: 'Open SmugMug in your browser',
      connect: 'Connect',
      reopen: 'Open again',
      opening: 'Opening…',
      stepTwo: 'Approve access, then paste the six-digit code SmugMug shows you',
      verifierPlaceholder: '123456',
      codeAfterConnect: 'Available once you have opened SmugMug in step 1.',
      finish: 'Finish connecting',
      finishing: 'Connecting…',
    },
  },
  manager: {
    title: 'Publish Manager',
    destinations: 'Destinations',
    loading: 'Loading…',
    save: 'Save',
    saving: 'Saving…',
    cancel: 'Cancel',
    status: {
      checking: 'Checking…',
      connected: 'Connected as {{account}}',
      notConnected: 'Not connected',
      notSetUp: 'Not set up',
    },
    account: {
      heading: 'Account',
      needsKey: 'Add your API key below, then connect your account here.',
      reconnect: 'Reconnect',
      disconnect: 'Disconnect',
      confirmDisconnect: {
        title: 'Disconnect {{destination}}?',
        message:
          'RapidRAW gives up its access to your {{destination}} account. Your API key, linked albums and publish history are kept, so reconnecting carries on where you left off.',
      },
    },
    apiKey: {
      heading: 'API key',
      change: 'Change',
    },
    output: {
      heading: 'Output',
      presetLabel: 'Export preset',
      placeholder: 'Choose a preset',
      unset: 'Choose the export preset photos are rendered with before they are sent to {{destination}}.',
      deleted: 'The preset this destination used has been deleted. Choose another.',
      noPresets: 'There are no export presets yet. Save one in the Export panel.',
      unsupportedFormat: '{{destination}} accepts {{formats}} only. Choose a preset that uses one of those.',
      manage: 'Manage presets in Export',
    },
    newAlbums: {
      heading: 'New albums',
      privacyLabel: 'Privacy',
      note: 'Applies only to albums RapidRAW creates. Albums that already exist on {{destination}} keep their privacy.',
      privacy: {
        Public: 'Public',
        Unlisted: 'Unlisted',
        Private: 'Private',
      },
    },
    impact: {
      title: 'Change the output preset?',
      message_one: '{{count}} photo already published to {{destination}}, in {{albums}}, used different settings.',
      message_other: '{{count}} photos already published to {{destination}}, in {{albums}}, used different settings.',
      albums_one: '{{count}} album',
      albums_other: '{{count}} albums',
      republish: 'Republish them next time',
      keep: 'Keep existing uploads',
    },
  },
  prompt: {
    notSetUp: '{{destination}} is not set up yet. Add your API key and connect your account in the Publish Manager.',
    setUp: 'Set up {{destination}}',
    notConnected: '{{destination}} is not connected.',
    connect: 'Connect {{destination}}',
  },
  connected: {
    heading: 'Account',
    account: 'Connected as {{account}}',
  },
  album: {
    heading: 'Album',
    placeholder: 'Choose an album',
    noAlbums: 'Create an album in the library first. Publishing works on albums, not folders.',
    mapping: 'Publishes to “{{name}}” at the top level of your SmugMug site, creating it if needed.',
    empty: 'This album has no photos.',
  },
  settings: {
    heading: 'Output',
    current: 'Publishing with your current export settings:',
    preset: 'Publishing with the “{{name}}” preset:',
    choosePreset: 'Choose an output preset',
    changePreset: 'Change in Publish Manager',
    quality: '{{quality}}% quality',
    resize: 'fit to {{value}} px',
    fullSize: 'full size',
    watermark: 'watermark',
    openExport: 'Change in the Export panel',
    unsupportedFormat: '{{destination}} accepts {{formats}} only. Choose one of those in the Export panel.',
    unsupportedPresetFormat: '{{destination}} accepts {{formats}} only. Choose a preset that uses one of those.',
  },
  preview: {
    heading: 'Changes',
    checking: 'Checking for changes…',
    counts: '{{unchanged}} unchanged, {{update}} to update, {{new}} new',
    unreadable_one: '{{count}} photo could not be read and will be reported as failed.',
    unreadable_other: '{{count}} photos could not be read and will be reported as failed.',
    failed: 'Could not check for changes: {{error}}',
  },
  actions: {
    publish_one: 'Publish {{count}} photo',
    publish_other: 'Publish {{count}} photos',
    upToDate: 'Album is up to date',
    starting: 'Starting…',
    cancel: 'Cancel publishing',
    cancelling: 'Cancelling…',
    done: 'Done',
  },
  progress: {
    heading: 'Publishing',
    count: '{{completed}} of {{total}}',
    preparing: 'Preparing photos…',
    completeHeading: 'Published',
    cancelledHeading: 'Publishing cancelled',
    errorHeading: 'Publishing stopped',
    summary: '{{uploaded}} new, {{updated}} updated, {{skipped}} unchanged',
    failedCount_one: '{{count}} failed',
    failedCount_other: '{{count}} failed',
    ambiguous_one: 'SmugMug did not confirm {{count}} photo. It will be tried again next time you publish.',
    ambiguous_other: 'SmugMug did not confirm {{count}} photos. They will be tried again next time you publish.',
    states: {
      skipped: 'Unchanged',
      uploaded: 'Uploaded',
      updated: 'Updated',
      failed: 'Failed',
      ambiguous: 'Unconfirmed',
    },
  },
  errors: {
    localFile: 'The photo could not be prepared on this computer.',
    presetMissing: 'Choose an output preset for this destination.',
  },
};

export type PublishTranslations = {
  publish: typeof publishResources;
};

/** Languages other than `en` are added here as translations arrive. */
const bundles: Record<string, typeof publishResources> = {
  en: publishResources,
};

export const registerPublishResources = () => {
  for (const [language, resources] of Object.entries(bundles)) {
    i18n.addResourceBundle(language, 'translation', { publish: resources }, true, true);
  }
};

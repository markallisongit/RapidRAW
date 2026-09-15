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
 *
 * Strings about the destination's albums use its own word for them. They are
 * called with `context: destinationId`, and a destination whose word differs
 * adds `_<id>` variants (SmugMug's albums are galleries); the rest fall back
 * to the plain key. Whole sentences, not an interpolated noun, so articles and
 * grammar stay right in every language.
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
      heading_smugmug: 'New galleries',
      privacyLabel: 'Privacy',
      note: 'Applies only to albums RapidRAW creates. Albums that already exist on {{destination}} keep their privacy.',
      note_smugmug:
        'Applies only to galleries RapidRAW creates. Galleries that already exist on {{destination}} keep their privacy.',
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
      albums_smugmug_one: '{{count}} gallery',
      albums_smugmug_other: '{{count}} galleries',
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
  destination: {
    noPreset: 'Choose the output preset photos are rendered with before publishing to {{destination}}.',
    presetDeleted: 'The output preset for {{destination}} has been deleted. Choose another before publishing.',
    choosePreset: 'Choose an output preset',
    unsupportedFormat: '{{destination}} accepts {{formats}} only. Choose a preset that uses one of those.',
  },
  refresh: {
    action: 'Refresh from {{destination}}',
    checked_one: '{{count}} album checked',
    checked_other: '{{count}} albums checked',
    checked_smugmug_one: '{{count}} gallery checked',
    checked_smugmug_other: '{{count}} galleries checked',
    renamed_one: '{{count}} renamed',
    renamed_other: '{{count}} renamed',
    broken_one: '{{count}} missing album',
    broken_other: '{{count}} missing albums',
    restored_one: '{{count}} restored',
    restored_other: '{{count}} restored',
    imagesMissing_one: '{{count}} photo missing',
    imagesMissing_other: '{{count}} photos missing',
    uploadNext_one: "it'll upload next time",
    uploadNext_other: "they'll upload next time",
    failed: 'Could not refresh: {{error}}',
  },
  links: {
    loading: 'Loading linked albums…',
    failed: 'Could not load linked albums: {{error}}',
    empty:
      'Link a RapidRAW album to an album on {{destination}}. Publishing then sends only the photos that are new or have changed.',
    empty_smugmug:
      'Link a RapidRAW album to a gallery on {{destination}}. Publishing then sends only the photos that are new or have changed.',
    publishAlbum: 'Publish an album…',
    publishThisAlbum: 'Publish “{{name}}”…',
    deletedLinks_one: '{{count}} link belongs to a deleted album',
    deletedLinks_other: '{{count}} links belong to deleted albums',
    removeDeleted: 'Remove',
    remoteName: '{{name}} on {{destination}}',
    actions: 'Actions for {{name}}',
    status: {
      upToDate: 'Up to date',
      changed_one: '{{count}} changed',
      changed_other: '{{count}} changed',
      new_one: '{{count}} new',
      new_other: '{{count}} new',
      settings_one: '{{count}} affected by settings',
      settings_other: '{{count}} affected by settings',
      broken: 'Not found on {{destination}}',
      notPublished: 'Not published yet',
      checking: 'Checking…',
      failed: 'Could not check',
    },
    menu: {
      publish: 'Publish',
      open: 'Open on {{destination}}',
      relink: 'Link to a different album…',
      relink_smugmug: 'Link to a different gallery…',
      recreate: 'Create it again',
      recreate_smugmug: 'Create it again',
      checkExisting: 'Check for photos already in this album…',
      checkExisting_smugmug: 'Check for photos already in this gallery…',
      unlink: 'Unlink',
    },
    confirmUnlink: {
      title: 'Unlink “{{name}}”?',
      message:
        'Its photos stay on {{destination}}. RapidRAW forgets what it published there, so linking this album again uploads every photo as new.',
    },
    confirmRemoveDeleted: {
      title_one: 'Remove the link to a deleted album?',
      title_other: 'Remove {{count}} links to deleted albums?',
      message_one:
        'Its RapidRAW album was deleted, so it can never be published again. Nothing is deleted on {{destination}}.',
      message_other:
        'Their RapidRAW albums were deleted, so they can never be published again. Nothing is deleted on {{destination}}.',
    },
    confirmRelink: {
      title: 'Link “{{name}}” to a different album?',
      title_smugmug: 'Link “{{name}}” to a different gallery?',
      message:
        'Its photos upload again, as new photos, into the album you choose. Nothing is removed from “{{remote}}” on {{destination}}.',
      message_smugmug:
        'Its photos upload again, as new photos, into the gallery you choose. Nothing is removed from “{{remote}}” on {{destination}}.',
      messageUnnamed:
        'Its photos upload again, as new photos, into the album you choose. Nothing is removed from its current album on {{destination}}.',
      messageUnnamed_smugmug:
        'Its photos upload again, as new photos, into the gallery you choose. Nothing is removed from its current gallery on {{destination}}.',
      confirm: 'Choose an album',
      confirm_smugmug: 'Choose a gallery',
    },
  },
  link: {
    title: 'Publish an album',
    relinkTitle: 'Link to a different album',
    relinkTitle_smugmug: 'Link to a different gallery',
    back: 'Back',
    cancel: 'Cancel',
    chooseAlbum: 'Choose a RapidRAW album',
    noAlbums: 'Create an album in the library first. Publishing works on albums, not folders.',
    linked: 'Linked',
    chooseTarget: 'Where should “{{name}}” go?',
    createNew: 'Create a new {{destination}} album',
    createNew_smugmug: 'Create a new {{destination}} gallery',
    linkExisting: 'Link to an existing album',
    linkExisting_smugmug: 'Link to an existing gallery',
    nameLabel: 'Album name',
    privacy: 'New albums are <strong>{{privacy}}</strong>.',
    privacy_smugmug: 'New galleries are <strong>{{privacy}}</strong>.',
    changePrivacy: 'Change in Publish Manager',
    create: 'Create and link',
    creating: 'Creating…',
    linkTo: 'Link to “{{name}}”',
    chooseRemote: 'Choose an album',
    chooseRemote_smugmug: 'Choose a gallery',
    linking: 'Linking…',
    nothingUploads: 'Nothing uploads until you click Publish.',
    alreadyExists: 'An album called “{{name}}” already exists on {{destination}}. Link to it instead?',
    alreadyExists_smugmug: 'A gallery called “{{name}}” already exists on {{destination}}. Link to it instead?',
    linkInstead: 'Link to it',
    alreadyLinked: 'That album is already linked to “{{name}}”.',
    alreadyLinked_smugmug: 'That gallery is already linked to “{{name}}”.',
    alreadyLinkedDeleted: 'That album is already linked to a RapidRAW album that has been deleted. Unlink it first.',
    alreadyLinkedDeleted_smugmug:
      'That gallery is already linked to a RapidRAW album that has been deleted. Unlink it first.',
  },
  existing: {
    title: 'Photos already in “{{name}}”',
    checking: 'Checking the album for photos…',
    checking_smugmug: 'Checking the gallery for photos…',
    comparing_one: 'Comparing {{count}} photo…',
    comparing_other: 'Comparing {{count}} photos…',
    cancel: 'Cancel',
    exact: {
      title_one: '{{count}} photo is already in “{{name}}”',
      title_other: '{{count}} photos are already in “{{name}}”',
      message_one:
        'It matches a photo in this RapidRAW album by file name. If it was edited since it was uploaded, it updates the next time it is edited.',
      message_other:
        'They match photos in this RapidRAW album by file name. Photos edited since they were uploaded update the next time they are edited.',
      adopt_one: 'Treat it as published',
      adopt_other: 'Treat them as published',
      uploadAgain_one: 'Upload it again',
      uploadAgain_other: 'Upload them again',
    },
    review: {
      title_one: '{{count}} photo looks like it is already in “{{name}}”',
      title_other: '{{count}} photos look like they are already in “{{name}}”',
      message:
        'Their file names differ from what publishing would call them. Check the pairs; a photo you tick is updated in place the next time you edit it.',
      open_one: 'Check the pair',
      open_other: 'Check the pairs',
      adopt_one: 'Treat {{count}} as published',
      adopt_other: 'Treat {{count}} as published',
      adoptNone: 'Tick the pairs to treat as published',
      uploadAgain: 'Upload them all again',
      close: 'Close',
      inRapidRaw: 'In RapidRAW',
      onDestination: 'On {{destination}}',
      tick: 'Treat {{name}} as published',
      noThumbnail: 'No preview',
      reason: {
        PublishName: 'Same file name',
        OriginalFileName: 'Same original file name',
        CaptureTime: 'Same capture time',
        LooksTheSame: 'Looks the same',
      },
      possibleNote: 'Unticked pairs rest on a single clue.',
    },
    adopting: 'Recording…',
    skipped_one:
      'Recorded {{recorded}}. {{count}} pair was skipped: its photo or the remote photo changed since the check.',
    skipped_other:
      'Recorded {{recorded}}. {{count}} pairs were skipped: their photos or the remote photos changed since the check.',
    noMatch_one: 'The photo in “{{name}}” does not match any in this album.',
    noMatch_other: 'None of the {{count}} photos in “{{name}}” match any in this album.',
    naming: 'Publishing names them like <code>{{example}}</code>, from the “{{preset}}” preset.',
    namingBoth:
      'Publishing names them like <code>{{example}}</code>; the album has names like <code>{{remote}}</code>.',
    namingBoth_smugmug:
      'Publishing names them like <code>{{example}}</code>; the gallery has names like <code>{{remote}}</code>.',
    allRecorded: 'Every photo in this album is already recorded as published to “{{name}}”.',
    empty: 'There are no photos in “{{name}}”.',
    noPreset: 'Choose an output preset to check for photos already in this album.',
    noPreset_smugmug: 'Choose an output preset to check for photos already in this gallery.',
    choosePreset: 'Choose a preset',
    failed: 'Checking “{{name}}” for photos failed: {{error}}',
    failedLinked: 'Linked, but checking “{{name}}” for photos failed: {{error}}',
    done: 'Done',
  },
  browser: {
    loading: 'Loading albums…',
    loading_smugmug: 'Loading galleries…',
    failed: 'Could not load albums: {{error}}',
    failed_smugmug: 'Could not load galleries: {{error}}',
    empty: 'Nothing here to link to.',
    linkedTo: 'Linked to “{{name}}”',
    linkedToDeleted: 'Linked to a deleted album',
  },
  settings: {
    quality: '{{quality}}% quality',
    fullSize: 'full size',
    watermark: 'watermark',
  },
  summary: {
    mapping: '{{album}} → {{destination}} “{{remote}}”',
    mappingUnnamed: '{{album}} → {{destination}}',
    new_one: '{{count}} new',
    new_other: '{{count}} new',
    update_one: '{{count}} to update',
    update_other: '{{count}} to update',
    settings_one: '{{count}} with changed settings',
    settings_other: '{{count}} with changed settings',
    unchanged_one: '{{count}} unchanged',
    unchanged_other: '{{count}} unchanged',
    empty: 'This album has no photos.',
    checking: 'Checking for changes…',
    failed: 'Could not check for changes: {{error}}',
    unreadable_one: '{{count}} photo could not be read and will be reported as failed.',
    unreadable_other: '{{count}} photos could not be read and will be reported as failed.',
    preset: 'Preset: {{name}} · {{output}}',
    broken: 'This album was not found on {{destination}}. Link it to a different album to publish it.',
    broken_smugmug: 'Its gallery was not found on {{destination}}. Link it to a different gallery to publish it.',
  },
  settingsChange: {
    title: 'Output settings changed',
    message_one: '{{count}} photo in this album was published with different settings.',
    message_other: '{{count}} photos in this album were published with different settings.',
    republish: 'Republish them too',
    keep: 'Only upload edited and new photos',
    cancel: 'Cancel',
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
  albumMenu: {
    publishTo: 'Publish to',
    publishNow: '{{destination}} — Publish now',
    publishNowCount_one: '{{destination}} — Publish now ({{count}} changed)',
    publishNowCount_other: '{{destination}} — Publish now ({{count}} changed)',
    linkAndPublish: '{{destination}} — Link and publish…',
  },
  deletedNotice: {
    title: 'Left on {{destination}}',
    one: '“{{album}}” was linked to the {{destination}} album “{{remote}}”, which has not been deleted.',
    one_smugmug: '“{{album}}” was linked to the {{destination}} gallery “{{remote}}”, which has not been deleted.',
    oneUnnamed: '“{{album}}” was linked to a {{destination}} album, which has not been deleted.',
    oneUnnamed_smugmug: '“{{album}}” was linked to a {{destination}} gallery, which has not been deleted.',
    many_other: '{{count}} deleted albums were linked to {{destination}} albums, which have not been deleted:',
    many_smugmug_other:
      '{{count}} deleted albums were linked to {{destination}} galleries, which have not been deleted:',
    line: '“{{album}}” → “{{remote}}”',
    lineUnnamed: '“{{album}}” → an album',
    lineUnnamed_smugmug: '“{{album}}” → a gallery',
    open: 'Open on {{destination}}',
    openShort: 'Open',
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

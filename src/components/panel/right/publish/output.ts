import type { TFunction } from 'i18next';

import { ExportPreset, FILE_FORMATS, FileFormat, FileFormats } from '../../../ui/ExportImportProperties';
import { DestinationInfo } from './usePublishState';

/** The Export panel's own record of its settings, never offered as a destination's preset. */
export const LAST_USED_PRESET_ID = '__last_used__';

/** The extension-to-MIME mapping `ExportPipeline::mime` applies in the backend. */
const MIME_BY_EXTENSION: Record<string, string> = {
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  png: 'image/png',
  tif: 'image/tiff',
  tiff: 'image/tiff',
  webp: 'image/webp',
  jxl: 'image/jxl',
};

const QUALITY_FORMATS: string[] = [FileFormats.Jpeg, FileFormats.Webp, FileFormats.Jxl];

type OutputValues = Omit<ExportPreset, 'id' | 'name'>;

/** Falls back to JPEG, as the Export panel and the backend's `output_format` do. */
export const formatOf = (fileFormat: string): FileFormat =>
  FILE_FORMATS.find((f) => f.id === fileFormat) ?? FILE_FORMATS[0];

/** One line: format, quality, size and watermark, e.g. "JPEG · 85% quality · Long Edge 2560 px". */
export const describeOutput = (s: OutputValues, t: TFunction): string => {
  const format = formatOf(s.fileFormat);
  const resizeModeLabels: Record<string, string> = {
    longEdge: t('export.resize.modes.longEdge'),
    shortEdge: t('export.resize.modes.shortEdge'),
    width: t('export.resize.modes.width'),
    height: t('export.resize.modes.height'),
  };
  return [
    format.name,
    QUALITY_FORMATS.includes(format.id) && t('publish.settings.quality', { quality: s.jpegQuality }),
    s.enableResize ? `${resizeModeLabels[s.resizeMode] ?? ''} ${s.resizeValue} px` : t('publish.settings.fullSize'),
    s.enableWatermark && s.watermarkPath && t('publish.settings.watermark'),
  ]
    .filter(Boolean)
    .join(' · ');
};

/** Whether the destination takes this format, and the names of those it does take. */
export const formatSupport = (destination: DestinationInfo | null, fileFormat: string) => {
  const acceptedMimes = destination?.capabilities.accepted_mime_types ?? [];
  const format = formatOf(fileFormat);
  return {
    // Unknown until the destination has loaded, which is not a reason to warn.
    isAccepted: acceptedMimes.length === 0 || acceptedMimes.includes(MIME_BY_EXTENSION[format.extensions[0]] ?? ''),
    acceptedNames: FILE_FORMATS.filter((f) => acceptedMimes.includes(MIME_BY_EXTENSION[f.extensions[0]]))
      .map((f) => f.name)
      .join(', '),
  };
};

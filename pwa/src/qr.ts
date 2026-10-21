import QRCode from 'qrcode'

/** SVG markup of a QR code (spec 6.1). The text is a URL the server built, never user input. */
export const qrSvg = (text: string): Promise<string> => QRCode.toString(text, { type: 'svg', margin: 1 })

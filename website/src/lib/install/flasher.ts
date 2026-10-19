/**
 * The Board on the visitor's USB serial port: esptool-js reads and writes its
 * flash, and after a reset the page follows its console.
 */

import type { ESPLoader, Transport } from "esptool-js";
import { LineReader } from "./console";
import {
  APP_DESCRIPTOR_OFFSET,
  APP_DESCRIPTOR_SIZE,
  barracudaVersion,
  firstApp,
  PARTITION_TABLE_OFFSET,
  PARTITION_TABLE_SIZE,
  parsePartitions,
  sameLayout,
} from "./flash-layout";
import type { FlashPart } from "./package";

/** What the installer learns from the connected chip. */
export type Chip = {
  /** esptool's name, such as `ESP32-S3`. */
  name: string;
  mac: string;
  flashSize?: string;
};

/** What the Board's flash already holds. */
export type Installed = {
  /** The Barracuda release on the Board, or `null` for other firmware. */
  version: string | null;
  /** Whether its partitions match the release, so an update keeps settings. */
  sameLayout: boolean;
};

/** Whether this browser can reach a USB serial port. */
export function serialSupported(): boolean {
  return (
    typeof navigator !== "undefined" &&
    "serial" in navigator &&
    typeof DecompressionStream !== "undefined"
  );
}

/** The release partition table, as the merged firmware image carries it. */
export function releaseTable(firmware: Uint8Array): Uint8Array {
  return firmware.subarray(
    PARTITION_TABLE_OFFSET,
    PARTITION_TABLE_OFFSET + PARTITION_TABLE_SIZE,
  );
}

/** The bytes the flasher writes, as a fraction of the whole install. */
function overall(parts: FlashPart[], index: number, fraction: number): number {
  const total = parts.reduce((sum, part) => sum + part.data.length, 0);
  const before = parts
    .slice(0, index)
    .reduce((sum, part) => sum + part.data.length, 0);
  return (before + fraction * parts[index].data.length) / total;
}

/** One Board on a serial port the visitor chose. */
export class SerialBoard {
  private transport?: Transport;
  private loader?: ESPLoader;
  private reader?: ReadableStreamDefaultReader<Uint8Array>;

  private constructor(private readonly port: SerialPort) {}

  /** Asks the visitor to pick the Board's port. */
  static async choose(): Promise<SerialBoard> {
    return new SerialBoard(await navigator.serial.requestPort());
  }

  /** Resets the chip into its bootloader and identifies it. */
  async connect(log: (line: string) => void): Promise<Chip> {
    await this.close();
    const { ESPLoader, Transport } = await import("esptool-js");
    const transport = new Transport(this.port, false);
    const loader = new ESPLoader({
      transport,
      baudrate: 921600,
      romBaudrate: 115200,
      terminal: {
        clean() {},
        writeLine: log,
        write: log,
      },
    });
    this.transport = transport;
    this.loader = loader;
    await loader.main();
    return {
      name: loader.chip.CHIP_NAME,
      mac: await loader.chip.readMac(loader),
      flashSize: await loader.detectFlashSize(),
    };
  }

  /** Reads which firmware the Board holds, against the release's table. */
  async inspect(release: Uint8Array): Promise<Installed> {
    const loader = this.connected();
    const table = await loader.readFlash(
      PARTITION_TABLE_OFFSET,
      PARTITION_TABLE_SIZE,
    );
    const app = firstApp(parsePartitions(table));
    const descriptor = app
      ? await loader.readFlash(
          app.offset + APP_DESCRIPTOR_OFFSET,
          APP_DESCRIPTOR_SIZE,
        )
      : new Uint8Array();
    return {
      version: barracudaVersion(descriptor),
      sameLayout: sameLayout(table, release),
    };
  }

  /**
   * Writes the parts, erasing the whole flash first for a fresh install,
   * then restarts the Board into the new firmware.
   */
  async write(
    parts: FlashPart[],
    options: {
      erase: boolean;
      onErased: () => void;
      onProgress: (part: number, fraction: number) => void;
    },
  ): Promise<void> {
    const loader = this.connected();
    if (options.erase) await loader.eraseFlash();
    options.onErased();
    await loader.writeFlash({
      fileArray: parts.map((part) => ({
        data: part.data,
        address: part.offset,
      })),
      flashMode: "keep",
      flashFreq: "keep",
      flashSize: "keep",
      eraseAll: false,
      compress: true,
      reportProgress: (index, written, total) =>
        options.onProgress(index, overall(parts, index, total ? written / total : 1)),
    });
    await loader.after("hard_reset");
    await this.close();
  }

  /**
   * Opens the console of the restarted Board and hands each line over,
   * until `stop` is called or the port goes away.
   */
  async follow(onLine: (line: string) => void, onLost: () => void) {
    await this.close();
    await this.port.open({ baudRate: 115200 });
    // Released DTR and RTS keep the chip running instead of in reset.
    await this.port.setSignals({
      dataTerminalReady: false,
      requestToSend: false,
    });
    const lines = new LineReader();
    let stopped = false;
    const pump = async () => {
      while (!stopped && this.port.readable) {
        this.reader = this.port.readable.getReader();
        try {
          for (;;) {
            const { value, done } = await this.reader.read();
            if (done) break;
            if (value) lines.push(value).forEach(onLine);
          }
        } catch {
          if (!stopped) onLost();
          return;
        } finally {
          this.reader.releaseLock();
          this.reader = undefined;
        }
      }
    };
    void pump();
    return async () => {
      stopped = true;
      await this.close();
    };
  }

  /** Releases the port. */
  async close(): Promise<void> {
    const transport = this.transport;
    this.transport = undefined;
    this.loader = undefined;
    try {
      if (transport) await transport.disconnect();
      else {
        await this.reader?.cancel();
        if (this.port.readable || this.port.writable) await this.port.close();
      }
    } catch {
      // The port may already be gone with the Board.
    }
  }

  private connected(): ESPLoader {
    if (!this.loader) throw new Error("the Board is not connected");
    return this.loader;
  }
}

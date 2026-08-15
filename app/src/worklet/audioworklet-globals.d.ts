// TS's DOM lib doesn't declare the AudioWorkletGlobalScope APIs
// (AudioWorkletProcessor/registerProcessor only exist inside a real
// worklet thread, not the main-thread DOM lib tsconfig.json includes).
// Same fix latent-mastering/latent-mixing both needed for their own
// worklets (T-012/T-102) -- without this, `npx tsc -b` fails on this file
// even though it's correct, since tsconfig includes all of src/.
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor(options?: any);
  process(
    inputs: Float32Array[][],
    outputs: Float32Array[][],
    parameters: Record<string, Float32Array>,
  ): boolean;
}

declare function registerProcessor(
  name: string,
  processorCtor: new (options?: any) => AudioWorkletProcessor,
): void;

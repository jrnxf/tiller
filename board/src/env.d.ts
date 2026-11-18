/// <reference types="vite/client" />

interface ImportMetaEnv {
  // The board a live board signs in from (vite.config.ts); empty otherwise.
  readonly VITE_TILLER_SIGN_IN_FROM?: string;
}

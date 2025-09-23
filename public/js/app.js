// js/app.js

import { fetchConfig } from './config.js';
import { setupTTL, setupUI } from './ui.js';
import { uploadHandler } from './upload.js';
import { ownedHandler } from './owned.js';
import { setupEventListeners } from './events.js';
import { applyother } from './other.js';

// Wait for the dynamic config to be fetched before initializing
fetchConfig().then(() => {
  // Apply all feature patches and other to the core handlers
  applyother(uploadHandler, ownedHandler);

  // Setup UI elements and initial state
  setupTTL();
  setupUI();

  // Load user's existing files
  ownedHandler.loadExisting();

  // Setup all event listeners (drag/drop, paste, etc.)
  setupEventListeners();
});
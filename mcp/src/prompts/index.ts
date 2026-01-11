/**
 * Prompts registry
 * Exports all available prompts
 */

import { PromptDefinition } from '../types/index.js';
import { addReactAnalyticsPrompt } from './add-react-analytics.js';

export const prompts: PromptDefinition[] = [addReactAnalyticsPrompt];

export { addReactAnalyticsPrompt };

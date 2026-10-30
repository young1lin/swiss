/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/* Process API contracts shared by the page and its sheets. */
                                                                                                            

/* The row of GET /api/process (SPEC §process.apps), camelCase on the wire. */
                             
               
                                                                     
                  
               
                  
                       
                       
                       
                         
                     
                
                          
                     
              
                      
                     
 

                                                                               

/* One child of the gateway (SPEC §process.children): stop goes through the
 * owner's hook, never a raw kill - a row without one shows no Stop. */
                                                                                                                                            

                                                                                                                                                                      

                                                                                                                              

/* The app entry as the config store holds it (SPEC §process.apps): argv, never a
 * string to split; env values stay references exactly as typed. */
                           
               
                                              
                                              
               
                   
                 
               
              
                      
                               
 
/* Wire shapes only: the page, the form and the logs share this one typed view of the API. */
